use super::{display_service, ConnInner, Connection, Subscriber};
use crate::virtual_display_manager::rustdesk_idd::ResolutionControl;
use base::message_proto::{DisplayInfo, Message, MessageBox};
use hbb_common::{
    anyhow::anyhow,
    bail, log,
    protobuf::Message as _,
    tokio::sync::{mpsc, oneshot},
    ResultType,
};
use std::{
    sync::Arc,
    thread::{Builder, JoinHandle},
};

pub(super) type Job = Box<dyn FnOnce() + Send>;
type Snapshot = ResultType<(Vec<DisplayInfo>, usize)>;

enum LoginDisplays {
    Waiting(oneshot::Receiver<ResultType<()>>),
    Ready(ResultType<()>),
}

#[derive(Default)]
pub(super) struct DisplayJobs {
    login: Option<LoginDisplays>,
    before_login: Vec<Message>,
    before_login_bytes: usize,
    resolutions: Option<Arc<()>>,
    sender: Option<mpsc::Sender<Job>>,
    worker: Option<(JoinHandle<()>, oneshot::Receiver<()>)>,
}

impl DisplayJobs {
    pub(super) fn submit(&mut self, job: Job) -> ResultType<()> {
        let sender = match &self.sender {
            Some(sender) => sender,
            None => {
                // Reconnect replays every peer connector, followed by privacy preparation.
                let capacity =
                    crate::virtual_display_manager::rustdesk_idd::PEER_INDICES.count() + 1;
                let (sender, mut receiver) = mpsc::channel::<Job>(capacity);
                let (done, stopped) = oneshot::channel();
                let thread = Builder::new()
                    .name("virtual-display".into())
                    .spawn(move || {
                        while let Some(job) = receiver.blocking_recv() {
                            if receiver.is_closed() {
                                break;
                            }
                            job();
                        }
                        drop(receiver);
                        let _ = done.send(());
                    })?;
                self.worker = Some((thread, stopped));
                self.sender.insert(sender)
            }
        };
        sender
            .try_send(job)
            .map_err(|error| anyhow!("Cannot queue virtual display request: {error}"))
    }

    pub(super) fn stop(&mut self) {
        self.login.take();
        drop(self.take_before_login());
        self.revoke_resolutions();
        self.sender.take();
    }

    pub(super) fn resolution_request(
        &mut self,
        change: impl FnOnce(&ResolutionControl) -> ResultType<()> + Send + 'static,
    ) -> impl FnOnce() -> ResultType<()> + Send + 'static {
        let permission = Arc::downgrade(self.resolutions.get_or_insert_with(Arc::default));
        move || {
            crate::privacy_mode::with_virtual_display_privacy(|privacy_active| {
                let control = ResolutionControl {
                    keyboard: permission,
                    privacy_active,
                };
                if !control.is_allowed() {
                    return Ok(());
                }
                change(&control)
            })
        }
    }

    pub(super) fn revoke_resolutions(&mut self) {
        self.resolutions.take();
    }

    pub(super) async fn finish(&mut self) {
        self.stop();
        if let Some((_, stopped)) = &mut self.worker {
            let _ = stopped.await;
        }
        self.join();
    }

    fn join(&mut self) {
        if let Some((thread, _)) = self.worker.take() {
            if thread.join().is_err() {
                log::error!("Virtual display worker terminated unexpectedly");
            }
        }
    }
}

impl Drop for DisplayJobs {
    fn drop(&mut self) {
        self.stop();
        self.join();
    }
}

impl DisplayJobs {
    pub(super) fn defer_until_login(&mut self, msg: Message) -> ResultType<()> {
        // Only pre-response traffic is buffered. Keep the same budget as a login frame.
        let size =
            usize::try_from(msg.compute_size())?.saturating_add(std::mem::size_of::<Message>());
        let total = self.before_login_bytes.saturating_add(size);
        if total > super::MAX_UNAUTHORIZED_MESSAGE {
            bail!("Too much data received before the login display was ready");
        }
        self.before_login.push(msg);
        self.before_login_bytes = total;
        Ok(())
    }

    pub(super) fn take_before_login(&mut self) -> Vec<Message> {
        self.before_login_bytes = 0;
        std::mem::take(&mut self.before_login)
    }

    fn queue_headless_login(
        &mut self,
        prepare: impl FnOnce() -> ResultType<()> + Send + 'static,
    ) -> LoginDisplays {
        let (sender, ready) = oneshot::channel();
        match self.submit(Box::new(move || {
            if !sender.is_closed() {
                let _ = sender.send(prepare());
            }
        })) {
            Ok(()) => LoginDisplays::Waiting(ready),
            Err(error) => LoginDisplays::Ready(Err(error)),
        }
    }

    pub(super) fn login_pending(&self) -> bool {
        matches!(self.login, Some(LoginDisplays::Waiting(_)))
    }

    pub(super) fn poll_login(&mut self) -> bool {
        let Some(LoginDisplays::Waiting(receiver)) = &mut self.login else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return false,
            Err(error) => Err(error.into()),
        };
        self.login = Some(LoginDisplays::Ready(result));
        true
    }

    pub(super) fn login_displays(&mut self) -> Option<Snapshot> {
        let preparation = if let Some(preparation) = self.login.take() {
            preparation
        } else {
            super::try_activate_screen();
            if crate::virtual_display_manager::is_amyuni_idd()
                || !crate::virtual_display_manager::is_virtual_display_supported()
                || !crate::platform::is_installed()
            {
                return Some(display_service::get_sync_displays_on_login());
            }
            match display_service::connected_login_displays() {
                Ok(Some(snapshot)) => return Some(Ok(snapshot)),
                Ok(None) => self.queue_headless_login(|| {
                    display_service::try_get_displays_add_amyuni_headless().map(|_| ())
                }),
                Err(error) => LoginDisplays::Ready(Err(error)),
            }
        };
        match preparation {
            LoginDisplays::Ready(result) => Some(display_service::current_login_displays(result)),
            waiting @ LoginDisplays::Waiting(_) => {
                self.login = Some(waiting);
                None
            }
        }
    }
}

impl Connection {
    pub(super) async fn complete_display_login(&mut self) -> bool {
        use crate::privacy_mode::{get_privacy_mode_conn_id, INVALID_PRIVACY_MODE_CONN_ID};

        if !self.check_privacy_mode_on().await {
            self.on_close("privacy mode", false).await;
            return false;
        }
        if !self.check_trusted_device().await || !self.send_authorized_login_response().await {
            return false;
        }
        for msg in self.display_jobs.take_before_login() {
            if get_privacy_mode_conn_id().is_some_and(|owner| {
                owner != INVALID_PRIVACY_MODE_CONN_ID && owner != self.inner.id()
            }) {
                self.on_close("privacy mode", false).await;
                return false;
            }
            if !self.on_message(msg).await {
                return false;
            }
        }
        true
    }

    pub(super) fn dispatch_display(
        &mut self,
        operation: impl FnOnce() -> ResultType<()> + Send + 'static,
    ) {
        let mut peer = self.inner.clone();
        let job = move || {
            if let Err(error) = operation() {
                report(&mut peer, error);
            }
        };
        if crate::virtual_display_manager::is_amyuni_idd() {
            job();
        } else if let Err(error) = self.display_jobs.submit(Box::new(job)) {
            report(&mut self.inner, error);
        }
    }
}

fn report(peer: &mut ConnInner, error: hbb_common::anyhow::Error) {
    hbb_common::throttled_log!(
        std::time::Duration::from_secs(1),
        error,
        "Virtual display: {error:#}"
    );
    let mut msg = Message::new();
    msg.set_message_box(MessageBox {
        msgtype: "nook-nocancel-hasclose".into(),
        title: "Virtual display".into(),
        text: format!("{error:#}"),
        ..Default::default()
    });
    peer.send(msg.into());
}

#[cfg(test)]
mod tests;
