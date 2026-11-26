use super::{ConnInner, Connection, Subscriber};
use base::message_proto::{Message, MessageBox};
use hbb_common::{
    anyhow::anyhow,
    log,
    tokio::sync::{mpsc, oneshot},
    ResultType,
};
use std::thread::{Builder, JoinHandle};

pub(super) type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
pub(super) struct DisplayJobs {
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
        self.sender.take();
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
