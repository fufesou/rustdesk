use super::{display_service, ConnInner, Connection, Subscriber};
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
    Ready(Snapshot),
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
        change: impl FnOnce(bool) -> ResultType<()> + Send + 'static,
    ) -> impl FnOnce() -> ResultType<()> + Send + 'static {
        let permission = Arc::downgrade(self.resolutions.get_or_insert_with(Arc::default));
        move || {
            crate::privacy_mode::with_virtual_display_privacy(|private| {
                if permission.strong_count() == 0 {
                    return Ok(());
                }
                change(private)
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

    pub(super) fn prepare_login(&mut self) -> bool {
        if crate::virtual_display_manager::is_amyuni_idd()
            || !crate::virtual_display_manager::is_virtual_display_supported()
            || !crate::platform::is_installed()
        {
            return false;
        }
        super::try_activate_screen();
        self.login = Some(match display_service::existing_login_displays() {
            Ok(Some(displays)) => LoginDisplays::Ready(Ok(displays)),
            Ok(None) => self.queue_headless_login(|| {
                display_service::try_get_displays_add_amyuni_headless().map(|_| ())
            }),
            Err(error) => LoginDisplays::Ready(Err(error)),
        });
        self.login_pending()
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
            Ok(result) => display_service::current_login_displays(result),
            Err(oneshot::error::TryRecvError::Empty) => return false,
            Err(error) => Err(error.into()),
        };
        self.login = Some(LoginDisplays::Ready(result));
        true
    }

    pub(super) fn login_displays(&mut self) -> Snapshot {
        match self.login.take() {
            Some(LoginDisplays::Ready(result)) => result,
            Some(LoginDisplays::Waiting(_)) => {
                Err(anyhow!("The login display is still being prepared"))
            }
            None => {
                super::try_activate_screen();
                display_service::get_sync_displays_on_login()
            }
        }
    }
}

impl Connection {
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
mod tests {
    use super::*;
    use hbb_common::tokio;

    #[test]
    fn virtual_display_login_preserves_pipelined_messages_within_the_login_budget() {
        let mut jobs = DisplayJobs::default();
        let mut first = Message::new();
        first.set_message_box(MessageBox {
            text: "first".into(),
            ..Default::default()
        });
        let second = Message::new();
        jobs.defer_until_login(first.clone()).unwrap();
        jobs.defer_until_login(second.clone()).unwrap();
        let mut oversized = Message::new();
        oversized.set_message_box(MessageBox {
            text: "x".repeat(super::super::MAX_UNAUTHORIZED_MESSAGE),
            ..Default::default()
        });
        assert!(jobs.defer_until_login(oversized).is_err());
        assert_eq!(jobs.take_before_login(), [first, second]);
    }

    #[tokio::test]
    async fn virtual_display_close_cancels_a_pending_headless_login() {
        let mut jobs = DisplayJobs::default();
        let (started, ready) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        jobs.login = Some(jobs.queue_headless_login(move || {
            started.send(()).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            Ok(())
        }));
        ready.await.unwrap();
        assert!(jobs.login_pending());
        assert!(!jobs.poll_login());
        jobs.stop();
        release.send(()).unwrap();
        jobs.finish().await;
        assert!(!jobs.poll_login());
        assert!(!jobs.login_pending());
    }

    #[test]
    fn virtual_display_resolution_revocation_only_cancels_earlier_requests() {
        let mut jobs = DisplayJobs::default();
        let (sent, received) = std::sync::mpsc::channel();
        let first = sent.clone();
        let old = jobs.resolution_request(move |_| Ok(first.send(1)?));
        jobs.revoke_resolutions();
        let new = jobs.resolution_request(move |_| Ok(sent.send(2)?));
        old().unwrap();
        new().unwrap();
        assert_eq!(received.into_iter().collect::<Vec<_>>(), [2]);
    }

    #[tokio::test]
    async fn virtual_display_requests_return_while_the_driver_is_waiting_and_run_in_order() {
        let mut queue = DisplayJobs::default();
        let (release, wait) = std::sync::mpsc::channel();
        let (sent, mut received) = mpsc::unbounded_channel();
        let first = sent.clone();
        queue
            .submit(Box::new(move || {
                wait.recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                first.send(1).unwrap();
            }))
            .unwrap();
        queue
            .submit(Box::new(move || sent.send(2).unwrap()))
            .unwrap();
        release.send(()).unwrap();
        assert_eq!(received.recv().await, Some(1));
        assert_eq!(received.recv().await, Some(2));
        queue.finish().await;
    }

    #[tokio::test]
    async fn virtual_display_disconnect_discards_queued_changes_before_cleanup() {
        let mut queue = DisplayJobs::default();
        let (started, ready) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        queue
            .submit(Box::new(move || {
                started.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
            }))
            .unwrap();
        ready.await.unwrap();
        let (applied, result) = oneshot::channel();
        queue
            .submit(Box::new(move || applied.send(()).unwrap()))
            .unwrap();
        queue.stop();
        let finish = queue.finish();
        tokio::pin!(finish);
        assert!(hbb_common::futures::poll!(&mut finish).is_pending());
        release.send(()).unwrap();
        finish.await;
        assert!(result.await.is_err());
    }
}
