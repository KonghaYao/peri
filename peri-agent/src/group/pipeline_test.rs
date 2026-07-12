use super::*;
use crate::messages::BaseMessage;
use crate::session::queue::{MessageKind, MessageSource};

fn make_queued(text: &str) -> QueuedMessage {
    QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(text.to_string()),
    )
}

#[test]
fn test_agent_id_unique() {
    let id1 = AgentId::new();
    let id2 = AgentId::new();
    assert_ne!(id1, id2, "每次 new() 应生成不同 ID");
    assert_ne!(id1.as_uuid(), uuid::Uuid::nil());
}

#[test]
fn test_agent_id_default() {
    let id = AgentId::default();
    assert_ne!(id.as_uuid(), uuid::Uuid::nil());
}

#[test]
fn test_register_and_list() {
    let pipeline = AgentPipeline::new();
    assert!(pipeline.is_empty());

    let id = AgentId::new();
    let _rx = pipeline.register(id);

    assert_eq!(pipeline.len(), 1);
    let ids = pipeline.list();
    assert!(ids.contains(&id));
}

#[test]
fn test_unregister_removes_mailbox() {
    let pipeline = AgentPipeline::new();
    let id = AgentId::new();
    let _rx = pipeline.register(id);

    pipeline.unregister(id);
    assert!(pipeline.is_empty());
}

#[test]
fn test_send_to_registered_agent() {
    let pipeline = AgentPipeline::new();
    let id = AgentId::new();
    let mut rx = pipeline.register(id);

    let msg = make_queued("hello");
    assert!(pipeline.send(id, msg).is_ok());

    let received = rx.try_recv().expect("应收到消息");
    assert_eq!(received.message.content(), "hello");
}

#[test]
fn test_send_to_unregistered_agent_fails() {
    let pipeline = AgentPipeline::new();
    let ghost = AgentId::new();
    let result = pipeline.send(ghost, make_queued("hi"));

    assert!(result.is_err(), "未注册 Agent 应返回错误");
    let err = result.unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn test_broadcast_reaches_all() {
    let pipeline = AgentPipeline::new();

    let id_a = AgentId::new();
    let id_b = AgentId::new();
    let mut rx_a = pipeline.register(id_a);
    let mut rx_b = pipeline.register(id_b);

    pipeline.broadcast(make_queued("announce"));

    let recv_a = rx_a.try_recv().expect("Agent A 应收到广播");
    let recv_b = rx_b.try_recv().expect("Agent B 应收到广播");
    assert_eq!(recv_a.message.content(), "announce");
    assert_eq!(recv_b.message.content(), "announce");
}

#[test]
fn test_broadcast_skips_dropped_mailbox() {
    let pipeline = AgentPipeline::new();

    let id_a = AgentId::new();
    let id_b = AgentId::new();
    let mut rx_a = pipeline.register(id_a);
    let _rx_b = pipeline.register(id_b);

    // 注销 id_b 后广播不应 panic
    pipeline.unregister(id_b);
    pipeline.broadcast(make_queued("partial"));

    let recv_a = rx_a.try_recv().expect("Agent A 应收到广播");
    assert_eq!(recv_a.message.content(), "partial");
}

#[test]
fn test_send_after_unregister_fails() {
    let pipeline = AgentPipeline::new();
    let id = AgentId::new();
    let _rx = pipeline.register(id);

    pipeline.unregister(id);
    let result = pipeline.send(id, make_queued("late"));
    assert!(result.is_err());
}
