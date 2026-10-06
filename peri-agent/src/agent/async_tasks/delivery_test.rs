use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::session::{QueuedPayload, SessionInbox};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

#[tokio::test]
async fn terminal_shell_receipt_wakes_idle_inbox_as_defer() {
    assert_terminal_receipt_wakes(MessageSource::ShellComplete, ReminderSeverity::Info).await;
}

#[tokio::test]
async fn terminal_shell_error_wakes_idle_inbox_as_defer() {
    assert_terminal_receipt_wakes(MessageSource::ShellComplete, ReminderSeverity::Error).await;
}

#[tokio::test]
async fn terminal_subagent_receipt_wakes_idle_inbox_as_defer() {
    assert_terminal_receipt_wakes(MessageSource::SubAgentComplete, ReminderSeverity::Info).await;
}

#[tokio::test]
async fn terminal_subagent_error_wakes_idle_inbox_as_defer() {
    assert_terminal_receipt_wakes(MessageSource::SubAgentComplete, ReminderSeverity::Error).await;
}

async fn assert_terminal_receipt_wakes(source: MessageSource, severity: ReminderSeverity) {
    let bound = TestSession::open().await;
    crate::session::test_resources::mock::work::bind_fixture_task(
        bound.resources(),
        &bound.thread_id(),
        1,
        "terminal-fixture-task",
    )
    .await;
    let transcript = Arc::new(parking_lot::RwLock::new(
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id()),
    ));
    let queue = MessageQueue::new();
    let inbox = Arc::new(SessionInbox::new(Arc::new(queue.clone())));
    let delivery = SessionTerminalDelivery::for_transcript(&transcript, &queue, Some(1)).unwrap();
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource(if source == MessageSource::ShellComplete {
                "shell".into()
            } else {
                "subagent".into()
            }),
            kind: if severity == ReminderSeverity::Error {
                "failed".into()
            } else {
                "completed".into()
            },
            severity,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "task finished".into(),
            summary: None,
            metadata: serde_json::json!({"task_id":"terminal-fixture-task"}),
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
        .deliver(delivery_id, &reminder, source.clone())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("Defer must wake the idle inbox")
        .unwrap();
    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, source);
    assert_eq!(messages[0].delivery_id, Some(delivery_id));
    assert!(matches!(
        messages[0].payload,
        QueuedPayload::SystemReminder(_)
    ));
    assert_eq!(reminder.as_reminder().severity, severity);
    let snapshot = bound
        .resources
        .inspect_work(&WorkQuery::new(
            bound.thread_id(),
            WorkSelector::Delivery {
                delivery_id: delivery_id.as_uuid().to_string(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Deliveries(deliveries) = snapshot.page else {
        panic!("delivery page required")
    };
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].obligation, ObligationStatus::Pending);
    assert!(bound
        .resources
        .load_session_history(&bound.thread_id())
        .await
        .unwrap()
        .is_empty());
}
