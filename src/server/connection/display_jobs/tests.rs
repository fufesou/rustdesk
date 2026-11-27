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
    assert!(jobs.login_displays().is_none());
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
