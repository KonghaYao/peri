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
fn test_drain_for_receive_consumes_prompt_info_keeps_defer() {
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

    let consumed = q.drain_for_receive();
    assert_eq!(consumed.len(), 2, "Receive 应消费 Prompt + Info");
    assert_eq!(consumed[0].message.content(), "p1");
    assert_eq!(consumed[1].message.content(), "i1");

    // Defer 保留在队列
    assert_eq!(q.len(), 1, "Defer 应保留");
}

#[test]
fn test_drain_for_end_returns_none_when_only_info() {
    let q = MessageQueue::new();
    q.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        make_msg("i1"),
    ));

    let result = q.drain_for_end();
    assert!(result.is_none(), "仅有 Info 时不应唤醒");
    assert_eq!(q.len(), 1, "Info 应保留");
}

#[test]
fn test_drain_for_end_wakes_on_defer() {
    let q = MessageQueue::new();
    q.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        make_msg("i1"),
    ));
    q.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        make_msg("d1"),
    ));

    let result = q.drain_for_end().expect("Defer 应唤醒");
    assert_eq!(result.len(), 1, "应只消费 Defer");
    assert_eq!(result[0].message.content(), "d1");
    assert_eq!(q.len(), 1, "Info 应保留");
}

#[test]
fn test_drain_for_end_wakes_on_prompt() {
    let q = MessageQueue::new();
    q.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        make_msg("p1"),
    ));

    let result = q.drain_for_end().expect("Prompt 应唤醒");
    assert_eq!(result.len(), 1);
    assert_eq!(q.len(), 0);
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
