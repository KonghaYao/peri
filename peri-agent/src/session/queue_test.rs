use super::*;
use crate::messages::MessageContent;

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
    assert_eq!(consumed[0].message.content(), "p1");
    assert_eq!(consumed[1].message.content(), "d1");
    assert_eq!(consumed[2].message.content(), "i1");
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
