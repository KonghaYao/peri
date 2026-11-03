use super::{CronOwner, MessageSource};
use crate::session::{MessageKind, MessageQueue, MqFilter, SessionInbox};
use std::sync::Arc;
use std::time::Duration;

/// cron 触发经持久 MQ 入队：生产者拿到的是接纳结论，订阅者由唤醒提示重查事实。
#[tokio::test]
async fn cron_trigger_is_enqueued_and_wakes_the_inbox() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    let mut owner = CronOwner::new();
    let (trigger_tx, trigger_rx) = tokio::sync::mpsc::unbounded_channel();
    owner.start(
        trigger_rx,
        inbox.handle(),
        Arc::new(tokio_util::sync::CancellationToken::new()),
    );

    trigger_tx.send("tick".to_owned()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), queue.await_wake())
        .await
        .expect("cron trigger must wake the inbox");

    let entries = queue.query(MqFilter::default()).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, MessageKind::Defer);
    assert_eq!(entries[0].source, MessageSource::CronTrigger);
    let message = entries[0]
        .payload
        .as_message()
        .expect("cron trigger carries a message payload");
    assert!(message.content().contains("Cron triggered: tick"));
    owner.shutdown();
}
