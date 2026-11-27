use super::{display_service, ConnInner, Connection, Subscriber, SEC30};
use base::message_proto::{DisplayInfo, Message, MessageBox, TestDelay};
use hbb_common::{
    anyhow::anyhow,
    log,
    tokio::{
        self,
        sync::{mpsc, oneshot},
        time::Instant,
    },
    ResultType,
};
use std::{
    sync::Arc,
    thread::{Builder, JoinHandle},
};

pub(super) type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
pub(super) struct DisplayJobs {
    pub(super) login_heartbeat: Option<Instant>,
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
        self.revoke_resolutions();
        self.sender.take();
    }

    pub(super) fn resolution_request(
        &mut self,
        change: impl FnOnce() -> ResultType<()> + Send + 'static,
    ) -> impl FnOnce() -> ResultType<()> + Send + 'static {
        let permission = Arc::downgrade(self.resolutions.get_or_insert_with(Arc::default));
        move || {
            if permission.strong_count() == 0 {
                return Ok(());
            }
            change()
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

impl Connection {
    pub(super) async fn login_displays(&mut self) -> ResultType<(Vec<DisplayInfo>, usize)> {
        if crate::virtual_display_manager::is_amyuni_idd()
            || !crate::virtual_display_manager::is_virtual_display_supported()
            || !crate::platform::is_installed()
        {
            return display_service::update_get_sync_displays_on_login().await;
        }
        if let Some(displays) = display_service::existing_login_displays()? {
            return Ok(displays);
        }
        let (sender, mut ready) = oneshot::channel();
        self.display_jobs.submit(Box::new(move || {
            let _ = sender.send(display_service::get_sync_displays_on_login());
        }))?;
        let mut heartbeat = Message::new();
        heartbeat.set_test_delay(TestDelay::default());
        loop {
            tokio::select! {
                result = &mut ready => return result?,
                _ = tokio::time::sleep(SEC30 / 2) => {
                    self.stream.send(&heartbeat).await?;
                    self.display_jobs.login_heartbeat = Some(Instant::now());
                    self.last_test_delay = None;
                }
            }
        }
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
mod tests {
    use super::*;
    use hbb_common::tokio;

    #[test]
    fn virtual_display_resolution_revocation_only_cancels_earlier_requests() {
        let mut jobs = DisplayJobs::default();
        let (sent, received) = std::sync::mpsc::channel();
        let first = sent.clone();
        let old = jobs.resolution_request(move || Ok(first.send(1)?));
        jobs.revoke_resolutions();
        let new = jobs.resolution_request(move || Ok(sent.send(2)?));
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
