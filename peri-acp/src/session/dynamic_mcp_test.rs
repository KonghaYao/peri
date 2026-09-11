use super::*;
use peri_acp_types::dynamic_mcp::{
    DynamicMcpIncarnationId, DynamicMcpLogicalKey, DynamicMcpOperationId, DynamicMcpOperationState,
};
use peri_acp_types::session::{MessageQueue, QueuedPayload, SessionInbox};

fn make_instance(session_id: &str) -> DynamicMcpInstanceKey {
    DynamicMcpInstanceKey {
        logical: DynamicMcpLogicalKey {
            session_id: session_id.into(),
            server_name: "local-tools".into(),
        },
        incarnation_id: DynamicMcpIncarnationId::from_string("mcpinc_test"),
    }
}

fn make_sink() -> (
    SessionDynamicMcpNotificationSink,
    Arc<SessionInbox>,
    Arc<MessageQueue>,
) {
    let queue = Arc::new(MessageQueue::new());
    let inbox = Arc::new(SessionInbox::new(Arc::clone(&queue)));
    let sink = SessionDynamicMcpNotificationSink {
        session_id: "session-1".into(),
        inbox: Arc::downgrade(&inbox),
    };
    (sink, inbox, queue)
}

#[test]
fn test_lifecycle_notification_preserves_typed_reminder_and_instance() {
    let (sink, _inbox, queue) = make_sink();
    let instance = make_instance("session-1");
    let accepted = sink.notify(DynamicMcpNotification {
        session_id: "session-1".into(),
        instance_key: instance.clone(),
        operation_id: DynamicMcpOperationId::from_string("mcpop_test"),
        state: DynamicMcpOperationState::Ready,
        safe_summary: "Local tools ready".into(),
    });
    assert!(accepted);
    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, MessageKind::Info);
    assert_eq!(messages[0].source, MessageSource::DynamicMcpNotification);
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("生命周期通知必须保持 typed reminder，不能变为普通消息");
    };
    let reminder = reminder.as_reminder();
    assert_eq!(reminder.kind, "lifecycle_changed");
    assert_eq!(reminder.body, "Local tools ready");
    assert_eq!(reminder.source.0, "dynamic_mcp");
    assert_eq!(
        reminder.metadata["instance_key"],
        serde_json::json!(instance)
    );
}

#[test]
fn test_oauth_notification_keeps_authorization_url_in_body() {
    let (sink, _inbox, queue) = make_sink();
    let accepted = sink.notify_authorization_needed(
        &make_instance("session-1"),
        "flow-1",
        "https://example.test/authorize",
    );
    assert!(accepted);
    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("OAuth 通知必须保持 typed reminder");
    };
    let reminder = reminder.as_reminder();
    assert_eq!(reminder.kind, "oauth_authorization_required");
    assert_eq!(reminder.severity, ReminderSeverity::Warning);
    assert!(reminder.body.contains("https://example.test/authorize"));
    assert_eq!(
        reminder.metadata,
        serde_json::json!({"server_name": "local-tools", "flow_id": "flow-1"})
    );
}

#[test]
fn test_notification_rejects_other_session() {
    let (sink, _inbox, queue) = make_sink();
    let accepted = sink.notify_authorization_needed(
        &make_instance("session-2"),
        "flow-1",
        "https://example.test/authorize",
    );
    assert!(!accepted, "其他会话的通知不得进入此 inbox");
    assert!(queue.drain_all().is_empty());
}

#[test]
fn test_notification_sink_does_not_keep_closed_inbox_alive() {
    let (sink, inbox, queue) = make_sink();
    drop(inbox);
    let accepted = sink.notify_authorization_needed(
        &make_instance("session-1"),
        "flow-1",
        "https://example.test/authorize",
    );
    assert!(!accepted, "inbox owner 已释放时不得继续投递");
    assert!(sink.inbox.upgrade().is_none(), "sink 必须只持 weak 引用");
    assert!(queue.drain_all().is_empty());
}
