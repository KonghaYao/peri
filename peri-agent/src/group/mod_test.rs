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
fn test_agent_group_new() {
    let (group, _rx) = AgentGroup::new();
    assert!(group.is_empty());
}

#[test]
fn test_register_agent_returns_triple() {
    let (group, _rx) = AgentGroup::new();

    let (id, _mailbox_rx, token) = group.register_agent(
        Some("test-agent".to_string()),
        CancelPolicy::Independent,
        None,
    );

    assert_eq!(group.len(), 1);
    assert!(!token.is_cancelled());
    let ids = group.list_agents();
    assert!(ids.contains(&id));
}

#[test]
fn test_register_agent_independent_token() {
    let (group, _rx) = AgentGroup::new();
    let parent = Arc::new(CancellationToken::new());

    // Independent 策略：parent_token 被忽略
    let (_id, _rx, token) =
        group.register_agent(None, CancelPolicy::Independent, Some(parent.clone()));

    // 取消 parent 不影响子 Agent
    parent.cancel();
    assert!(
        !token.is_cancelled(),
        "Independent 模式下子 Agent 不应受父取消影响"
    );
}

#[test]
fn test_register_agent_cascade_token() {
    let (group, _rx) = AgentGroup::new();
    let parent = Arc::new(CancellationToken::new());

    // Cascade 策略：parent 取消时子 Agent 级联取消
    let (_id, _rx, token) = group.register_agent(None, CancelPolicy::Cascade, Some(parent.clone()));

    parent.cancel();
    assert!(token.is_cancelled(), "Cascade 模式下父取消应级联到子 Agent");
}

#[test]
fn test_register_agent_cascade_without_parent() {
    let (group, _rx) = AgentGroup::new();

    // Cascade 但无 parent_token，应回退到独立 token
    let (_id, _rx, token) = group.register_agent(None, CancelPolicy::Cascade, None);

    assert!(
        !token.is_cancelled(),
        "无 parent 时 Cascade 应创建独立 token"
    );
}

#[test]
fn test_destroy_agent_removes_and_unregisters() {
    let (group, _rx) = AgentGroup::new();
    let (id, _mailbox_rx, token) = group.register_agent(None, CancelPolicy::Independent, None);

    assert_eq!(group.len(), 1);

    group.destroy_agent(id);

    assert!(group.is_empty());
    assert!(
        token.is_cancelled(),
        "destroy_agent 应取消该 Agent 的 token"
    );
}

#[test]
fn test_send_to_registered_agent() {
    let (group, _rx) = AgentGroup::new();
    let (target_id, mut target_rx, _token) =
        group.register_agent(None, CancelPolicy::Independent, None);

    let msg = make_queued("hello from caller");
    assert!(group.send(target_id, msg).is_ok());

    let received = target_rx.try_recv().expect("目标 Agent 应收到消息");
    assert_eq!(received.message.content(), "hello from caller");
}

#[test]
fn test_send_to_nonexistent_agent_fails() {
    let (group, _rx) = AgentGroup::new();
    let ghost = AgentId::new();

    let result = group.send(ghost, make_queued("hi"));
    assert!(result.is_err());
}

#[test]
fn test_broadcast_to_all_agents() {
    let (group, _rx) = AgentGroup::new();

    let (_id_a, mut rx_a, _) =
        group.register_agent(Some("A".to_string()), CancelPolicy::Independent, None);
    let (_id_b, mut rx_b, _) =
        group.register_agent(Some("B".to_string()), CancelPolicy::Independent, None);

    group.broadcast(make_queued("announce"));

    let recv_a = rx_a.try_recv().expect("Agent A 应收到广播");
    let recv_b = rx_b.try_recv().expect("Agent B 应收到广播");
    assert_eq!(recv_a.message.content(), "announce");
    assert_eq!(recv_b.message.content(), "announce");
}

#[test]
fn test_cancel_agent() {
    let (group, _rx) = AgentGroup::new();
    let (id, _mailbox_rx, token) = group.register_agent(None, CancelPolicy::Independent, None);

    assert!(!token.is_cancelled());
    group.cancel_agent(id);
    assert!(token.is_cancelled());
}

#[test]
fn test_cancel_all() {
    let (group, _rx) = AgentGroup::new();

    let (_id_a, _rx_a, token_a) = group.register_agent(None, CancelPolicy::Independent, None);
    let (_id_b, _rx_b, token_b) = group.register_agent(None, CancelPolicy::Independent, None);

    group.cancel_all();

    assert!(token_a.is_cancelled());
    assert!(token_b.is_cancelled());
}

#[test]
fn test_get_agent() {
    let (group, _rx) = AgentGroup::new();
    let (id, _rx, _token) =
        group.register_agent(Some("finder".to_string()), CancelPolicy::Independent, None);

    let handle = group.get_agent(&id).expect("应找到已注册 Agent");
    assert_eq!(handle.agent_id, id);
    assert_eq!(handle.name.as_deref(), Some("finder"));
    assert_eq!(handle.cancel_policy, CancelPolicy::Independent);
}

#[test]
fn test_get_agent_nonexistent() {
    let (group, _rx) = AgentGroup::new();
    let ghost = AgentId::new();
    assert!(group.get_agent(&ghost).is_none());
}

#[test]
fn test_event_sender_clone() {
    let (group, _rx) = AgentGroup::new();
    let sender = group.event_sender();
    // 不 panic 即可——验证 clone 可用
    let _sender2 = sender.clone();
}

#[test]
fn test_send_via_pipeline_after_destroy_fails() {
    let (group, _rx) = AgentGroup::new();
    let (id, _mailbox_rx, _token) = group.register_agent(None, CancelPolicy::Independent, None);

    group.destroy_agent(id);

    let result = group.send(id, make_queued("late"));
    assert!(result.is_err(), "destroy 后发送应失败");
}

#[test]
fn test_multiple_agents_independent_lifecycle() {
    let (group, _rx) = AgentGroup::new();

    let (id_a, mut rx_a, token_a) =
        group.register_agent(Some("A".to_string()), CancelPolicy::Independent, None);
    let (id_b, mut rx_b, token_b) =
        group.register_agent(Some("B".to_string()), CancelPolicy::Independent, None);

    assert_eq!(group.len(), 2);

    // A 取消不影响 B
    group.cancel_agent(id_a);
    assert!(token_a.is_cancelled());
    assert!(!token_b.is_cancelled());

    // A 的 mailbox 已关闭，B 的仍可用
    let _ = rx_a.try_recv(); // 可能收到 None（channel closed）
    group.send(id_b, make_queued("still alive")).ok();
    assert!(rx_b.try_recv().is_ok(), "B 的 mailbox 应仍可用");
}
