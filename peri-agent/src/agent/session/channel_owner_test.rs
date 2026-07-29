//! 从 channel_owner.rs 分离的测试模块

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::agent::session::inbox::SessionInbox;
use crate::interaction::channel_types::ChannelNotification;
use crate::session::MessageQueue;

fn make_notif(source: &str, chat_id: &str, text: &str) -> ChannelNotification {
    ChannelNotification {
        source: source.to_string(),
        chat_id: chat_id.to_string(),
        text: text.to_string(),
    }
}

#[tokio::test]
async fn test_channel_owner_forwards_notification_to_inbox() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let inbox_handle = inbox.handle();

    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = tokio_util::sync::CancellationToken::new();

    let mut owner = ChannelOwner::new();
    owner.start(rx, inbox_handle, shutdown.clone());

    // Send a channel notification
    tx.send(make_notif(
        "plugin:weixin:weixin",
        "chat123",
        "hello from weixin",
    ))
    .unwrap();

    // Give the task time to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Should have one defer message in the queue
    assert_eq!(inbox.queue().len(), 1);

    // Verify it's a ChannelMessage defer
    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::ChannelMessage);

    shutdown.cancel();
    owner.shutdown();
}

#[tokio::test]
async fn test_channel_owner_stops_on_shutdown() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let inbox_handle = inbox.handle();

    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = tokio_util::sync::CancellationToken::new();

    let mut owner = ChannelOwner::new();
    owner.start(rx, inbox_handle, shutdown.clone());

    // Cancel should stop the loop
    shutdown.cancel();

    // Give time for the task to notice cancellation
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Sending after shutdown should not be received
    // (tx.send may fail if the task already dropped rx, which is expected)
    let _ = tx.send(make_notif("source", "chat", "should not arrive"));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Queue should be empty
    assert!(inbox.queue().is_empty());

    owner.shutdown();
}

#[tokio::test]
async fn test_channel_owner_stops_when_rx_closed() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let inbox_handle = inbox.handle();

    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = tokio_util::sync::CancellationToken::new();

    let mut owner = ChannelOwner::new();
    owner.start(rx, inbox_handle, shutdown);

    // Drop the sender — closes the channel
    drop(tx);

    // Task should exit on its own
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Queue should be empty
    assert!(inbox.queue().is_empty());
}

#[tokio::test]
async fn test_channel_owner_notification_format() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let inbox_handle = inbox.handle();

    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = tokio_util::sync::CancellationToken::new();

    let mut owner = ChannelOwner::new();
    owner.start(rx, inbox_handle, shutdown.clone());

    tx.send(make_notif("plugin:weixin:weixin", "chat42", "test msg"))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    let content = msgs[0].message.content();
    assert!(content.contains("plugin:weixin:weixin"));
    assert!(content.contains("chat42"));
    assert!(content.contains("test msg"));
    assert!(content.contains("<system-reminder>"));
    assert!(content.contains("<channel"));
    assert!(content.contains("</channel>"));
    assert!(content.contains("</system-reminder>"));

    shutdown.cancel();
    owner.shutdown();
}

#[tokio::test]
async fn test_channel_owner_default_and_debug() {
    let owner = ChannelOwner::default();
    assert!(!format!("{:?}", owner).contains("running: true"));

    let mut owner = ChannelOwner::new();
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let (_tx, rx) = mpsc::unbounded_channel();
    let shutdown = tokio_util::sync::CancellationToken::new();

    owner.start(rx, inbox.handle(), shutdown);
    assert!(format!("{:?}", owner).contains("running: true"));

    owner.shutdown();
    assert!(!format!("{:?}", owner).contains("running: true"));
}
