//! 从 cron_owner.rs 分离的测试模块
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::agent::session::inbox::SessionInbox;
use crate::session::MessageQueue;

#[tokio::test]
async fn test_cron_owner_forwards_trigger_to_inbox() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let inbox_handle = inbox.handle();

    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

    let mut owner = CronOwner::new();
    owner.start(rx, inbox_handle, shutdown.clone());

    // Send a trigger prompt
    tx.send("check deploy status".to_string()).unwrap();

    // Give the task time to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Should have one defer message in the queue
    assert_eq!(inbox.queue().len(), 1);

    // Verify it's a CronTrigger defer
    let drained = inbox.queue().drain_all();
    assert_eq!(drained.len(), 1, "应有一条消息");
    assert_eq!(drained[0].source, MessageSource::CronTrigger);

    shutdown.cancel();
    owner.shutdown();
}

#[tokio::test]
async fn test_cron_owner_stops_on_shutdown() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let inbox_handle = inbox.handle();

    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

    let mut owner = CronOwner::new();
    owner.start(rx, inbox_handle, shutdown.clone());

    // Cancel should stop the loop
    shutdown.cancel();

    // Give time for the task to notice cancellation
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Sending after shutdown should fail — receiver was dropped when task ended.
    // This proves the task actually stopped (rx no longer alive).
    assert!(
        tx.send("should not arrive".to_string()).is_err(),
        "sender should fail after receiver task ends"
    );

    // Queue should be empty — no trigger was sent before shutdown.
    assert!(inbox.queue().is_empty());

    owner.shutdown();
}

#[tokio::test]
async fn test_cron_owner_stops_when_rx_closed() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let inbox_handle = inbox.handle();

    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

    let mut owner = CronOwner::new();
    owner.start(rx, inbox_handle, shutdown);

    // Drop the sender — closes the channel
    drop(tx);

    // Task should exit on its own
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Queue should be empty
    assert!(inbox.queue().is_empty());
}

#[tokio::test]
async fn test_cron_owner_default_and_debug() {
    let owner = CronOwner::default();
    assert!(!format!("{:?}", owner).contains("running: true"));

    let mut owner = CronOwner::new();
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let (_tx, rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

    owner.start(rx, inbox.handle(), shutdown);
    assert!(format!("{:?}", owner).contains("running: true"));

    owner.shutdown();
    assert!(!format!("{:?}", owner).contains("running: true"));
}
