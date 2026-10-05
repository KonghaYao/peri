use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::session::{QueuedPayload, SessionInbox};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

#[tokio::test]
async fn terminal_shell_receipt_wakes_idle_inbox_as_defer() {
    let bound = TestSession::open().await;
    let transcript = Arc::new(parking_lot::RwLock::new(
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id()),
    ));
    let queue = MessageQueue::new();
    let inbox = Arc::new(SessionInbox::new(Arc::new(queue.clone())));
    let delivery =
        SessionTerminalDelivery::for_transcript(&transcript, &queue, Some(&inbox.handle()))
            .unwrap();
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("shell".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "shell done".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let waiting = tokio::spawn({
        let inbox = Arc::clone(&inbox);
        async move { inbox.await_wake().await }
    });
    tokio::task::yield_now().await;
    assert!(
        !waiting.is_finished(),
        "idle loop must wait before the receipt"
    );

    let delivery_id = peri_acp_types::messages::MessageId::new();
    delivery
        .deliver(delivery_id, &reminder, MessageSource::ShellComplete)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("Defer must wake the idle inbox")
        .unwrap();
    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::ShellComplete);
    assert_eq!(messages[0].delivery_id, Some(delivery_id));
    assert!(matches!(
        messages[0].payload,
        QueuedPayload::SystemReminder(_)
    ));
    assert_eq!(reminder.as_reminder().severity, ReminderSeverity::Info);
}
