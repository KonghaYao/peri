use super::*;
use crate::messages::{BaseMessage, MessageContent};
use peri_acp_types::system_reminder::{
    ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalSource, SystemReminder, TrustedSystemReminder,
    TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use serde_json::json;

fn make_reminder() -> TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalSource("test".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![
                peri_acp_types::system_reminder::ReminderAudience::Model,
            ]),
            body: "done".into(),
            summary: None,
            metadata: json!({"id": 1}),
        })
        .unwrap()
}

fn make_msg(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

#[test]
fn test_kind_wakes_up() {
    assert!(MessageKind::Prompt.wakes_up());
    assert!(MessageKind::Defer.wakes_up());
    assert!(!MessageKind::Info.wakes_up());
}

#[test]
fn test_drain_all_consumes_all_message_types() {
    // RCRA：drain_all 消费全部消息类型（Prompt + Info + Defer）
    let q = MessageQueue::new();
    q.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        make_msg("p1"),
    ));
    q.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        make_msg("d1"),
    ));
    q.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        make_msg("i1"),
    ));

    let consumed = q.drain_all();
    assert_eq!(consumed.len(), 3, "drain_all 应消费全部三种类型");
    assert!(matches!(
        &consumed[0].payload,
        QueuedPayload::Message(message) if message.content() == "p1"
    ));
    assert!(matches!(
        &consumed[1].payload,
        QueuedPayload::Message(message) if message.content() == "d1"
    ));
    assert!(matches!(
        &consumed[2].payload,
        QueuedPayload::Message(message) if message.content() == "i1"
    ));
    assert!(q.is_empty(), "队列应完全排空");
}

#[test]
fn test_has_wake_up_only_prompt_and_defer() {
    let q = MessageQueue::new();
    assert!(!q.has_wake_up(), "空队列不应唤醒");

    // Info 不唤醒
    q.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        make_msg("i1"),
    ));
    assert!(!q.has_wake_up(), "仅有 Info 时不应唤醒");

    // Defer 唤醒
    q.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        make_msg("d1"),
    ));
    assert!(q.has_wake_up(), "Defer 应唤醒");

    // drain_all 后队列为空
    q.drain_all();
    assert!(!q.has_wake_up(), "排空后不应唤醒");

    // Prompt 唤醒
    q.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        make_msg("p1"),
    ));
    assert!(q.has_wake_up(), "Prompt 应唤醒");
}

#[test]
fn test_reminder_payload_roundtrip_is_independent_of_wake_kind() {
    let q = MessageQueue::new();
    let reminder = make_reminder();
    q.push(QueuedMessage::system_reminder(
        MessageKind::Info,
        MessageSource::SystemInjected,
        reminder.clone(),
    ));
    assert!(!q.has_wake_up());

    q.push(QueuedMessage::system_reminder(
        MessageKind::Defer,
        MessageSource::SystemInjected,
        reminder.clone(),
    ));
    assert!(q.has_wake_up());

    let drained = q.drain_all();
    for queued in drained {
        let QueuedPayload::SystemReminder(actual) = queued.payload else {
            panic!("expected reminder payload");
        };
        assert_eq!(actual, reminder);
    }
}

#[test]
fn test_clear() {
    let q = MessageQueue::new();
    q.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        make_msg("p1"),
    ));
    q.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        make_msg("i1"),
    ));
    assert_eq!(q.len(), 2);

    q.clear();
    assert!(q.is_empty());
}

#[test]
fn test_push_batch_no_op_on_empty() {
    let q = MessageQueue::new();
    q.push_batch(vec![]);
    assert!(q.is_empty());
}

#[test]
fn test_has_pending_defer_matches_source_and_kind() {
    let q = MessageQueue::new();
    assert!(
        !q.has_pending_defer(&MessageSource::SubAgentComplete),
        "空队列无 pending Defer"
    );

    // SubAgentComplete Defer → 命中
    q.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        make_msg("d1"),
    ));
    assert!(q.has_pending_defer(&MessageSource::SubAgentComplete));
    // 其他来源不命中（shell/workflow 等不得被当作 bg agent 续跑依据）
    assert!(!q.has_pending_defer(&MessageSource::ShellComplete));
    assert!(!q.has_pending_defer(&MessageSource::WorkflowComplete));
    assert!(!q.has_pending_defer(&MessageSource::CronTrigger));

    // 非 Defer 消息不命中：仅 Info（同来源）
    q.push(QueuedMessage::info(
        MessageSource::SubAgentComplete,
        make_msg("i1"),
    ));
    assert!(
        q.has_pending_defer(&MessageSource::SubAgentComplete),
        "先前 Defer 仍在，依然命中"
    );
    q.drain_all();
    q.push(QueuedMessage::defer(
        MessageSource::ShellComplete,
        make_msg("shell done"),
    ));
    assert!(q.has_pending_defer(&MessageSource::ShellComplete));
    assert!(
        !q.has_pending_defer(&MessageSource::SubAgentComplete),
        "shell completion must not qualify as a subagent completion"
    );

    q.drain_all();
    q.push(QueuedMessage::info(
        MessageSource::SubAgentComplete,
        make_msg("i2"),
    ));
    assert!(
        !q.has_pending_defer(&MessageSource::SubAgentComplete),
        "仅 Info 不命中（Info 不 wake 新 turn）"
    );

    // Prompt（同来源）不命中
    q.push(QueuedMessage::prompt(
        MessageSource::SubAgentComplete,
        make_msg("p1"),
    ));
    assert!(
        !q.has_pending_defer(&MessageSource::SubAgentComplete),
        "Prompt 不命中（仅 Defer kind）"
    );

    // drain 后清空
    q.drain_all();
    assert!(!q.has_pending_defer(&MessageSource::SubAgentComplete));
}
