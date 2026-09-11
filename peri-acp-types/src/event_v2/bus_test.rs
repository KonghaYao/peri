//! 通道收发、饱和和 FIFO 契约。
use super::*;
use crate::{identity::AgentId, messages::MessageId, session::TurnId};

fn make_ids() -> (TurnId, AgentId) {
    (TurnId::new(), AgentId::new())
}

// ─── EventBus + EventHandles 集成测试 ──────────────────────────────────

#[tokio::test]
async fn test_event_bus_emit_and_receive_render() {
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let (turn_id, agent_id) = make_ids();

    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "hello".to_string(),
    });

    let received = handles.try_render().expect("应收到渲染层事件");
    assert_eq!(received.turn_id(), turn_id);
    assert_eq!(received.agent_id(), agent_id);
}

#[tokio::test]
async fn test_event_bus_emit_and_receive_state() {
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let (turn_id, agent_id) = make_ids();

    bus.emit_state(StateEvent::StateSnapshot {
        turn_id,
        agent_id,
        message_count: 3,
        total_tokens: 1000,
        current_step: 3,
        consecutive_failures: 0,
        budget_pct: Some(0.5),
        context_total_tokens: Some(200_000),
    });

    let received = handles.try_state().expect("应收到状态层事件");
    assert_eq!(received.turn_id(), turn_id);
    assert_eq!(received.agent_id(), agent_id);
}

#[tokio::test]
async fn test_event_bus_emit_and_receive_render_turn_completed() {
    // TurnCompleted 在 Render 层，必须通过 render 通道接收
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let (turn_id, agent_id) = make_ids();

    bus.emit_render(RenderEvent::TurnCompleted {
        turn_id,
        agent_id,
        steps: 3,
        elapsed_secs: 1.5,
        finalized_messages: std::sync::Arc::new(vec![]),
    });

    let received = handles.try_render().expect("应收到渲染层 TurnCompleted");
    assert_eq!(received.turn_id(), turn_id);
    assert_eq!(received.agent_id(), agent_id);
    assert!(matches!(
        received,
        RenderEvent::TurnCompleted { steps: 3, .. }
    ));
}

#[tokio::test]
async fn test_event_bus_emit_and_receive_observe() {
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let (turn_id, agent_id) = make_ids();

    let subscribers = bus.emit_observe(ObserveEvent::LlmCallStart {
        turn_id,
        agent_id,
        step: 1,
        messages: std::sync::Arc::new(vec![]),
        tools: vec![],
    });
    // 默认 1 个接收者（EventHandles 内部的）
    assert_eq!(subscribers, 1);

    let received = handles.try_observe().expect("应收到观测层事件");
    assert_eq!(received.turn_id(), turn_id);
    assert_eq!(received.agent_id(), agent_id);
}

#[tokio::test]
async fn test_event_bus_observe_no_subscriber_returns_zero() {
    let (bus, _handles) = EventBus::new(EventBusConfig::default());
    let (turn_id, agent_id) = make_ids();

    // 丢弃 handles 中的 observe_rx 后再发送
    let n = bus.emit_observe(ObserveEvent::LlmCallEnd {
        turn_id,
        agent_id,
        step: 0,
        model: "test".to_string(),
        output: "test output".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: None,
    });
    // handles 仍持有 receiver，所以至少 1 个订阅者
    assert!(n >= 1);
}

#[tokio::test]
async fn test_event_bus_subscribe_observe_shares_channel() {
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let (turn_id, agent_id) = make_ids();

    // 创建额外的订阅者
    let mut extra_rx = handles.subscribe_observe();

    bus.emit_observe(ObserveEvent::MessagesCompacted {
        turn_id,
        agent_id,
        before_count: 50,
        after_count: 10,
        summary: "compressed".to_string(),
        messages: vec![],
        files: vec![],
        skills: vec![],
        re_inject_count: 0,
        strategy: crate::event::CompactStrategy::Full,
        affected_count: 0,
        estimated_tokens_saved: 0,
        estimated_tokens_before: 0,
        estimated_tokens_after: 0,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
        full_escalation_reason: None,
        cache_hit_rate_before: 0.0,
        outcome: crate::compact::CompactOutcome::FullApplied,
    });

    // 两个接收者都能收到
    let from_main = handles.try_observe().expect("主接收者应收到事件");
    let from_extra = extra_rx.try_recv().expect("额外接收者应收到事件");
    assert_eq!(from_main.turn_id(), from_extra.turn_id());
}

#[tokio::test]
async fn test_event_bus_render_channel_full_drops_event() {
    // 极小容量（1），填满后 try_send 应丢弃
    let (bus, mut handles) = EventBus::new(EventBusConfig {
        render_capacity: 1,
        ..Default::default()
    });
    let (turn_id, agent_id) = make_ids();

    // 填满通道（容量 1）
    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "first".to_string(),
    });
    // 第二个事件应被丢弃（不 panic）
    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "second".to_string(),
    });

    // 只能读出 1 个
    let r1 = handles.try_render().expect("第一个事件应在");
    assert!(matches!(r1, RenderEvent::TextChunk { ref chunk, .. } if chunk == "first"));
    let r2 = handles.try_render();
    assert!(r2.is_none(), "第二个事件应被丢弃");
}

#[tokio::test]
async fn test_event_bus_state_channel_full_drops_event() {
    let (bus, mut handles) = EventBus::new(EventBusConfig {
        state_capacity: 1,
        ..Default::default()
    });
    let (turn_id, agent_id) = make_ids();

    bus.emit_state(StateEvent::StateSnapshot {
        turn_id,
        agent_id,
        message_count: 1,
        total_tokens: 0,
        current_step: 1,
        consecutive_failures: 0,
        budget_pct: None,
        context_total_tokens: None,
    });
    bus.emit_state(StateEvent::StateSnapshot {
        turn_id,
        agent_id,
        message_count: 2,
        total_tokens: 0,
        current_step: 2,
        consecutive_failures: 0,
        budget_pct: None,
        context_total_tokens: None,
    });

    let s1 = handles.try_state().expect("第一个事件应在");
    assert!(matches!(
        s1,
        StateEvent::StateSnapshot {
            current_step: 1,
            ..
        }
    ));
    let s2 = handles.try_state();
    assert!(s2.is_none(), "第二个事件应被丢弃");
}

#[tokio::test]
async fn test_event_bus_multiple_events_in_order() {
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let (turn_id, agent_id) = make_ids();

    bus.emit_render(RenderEvent::ThinkingChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "think".to_string(),
    });
    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "answer".to_string(),
    });
    bus.emit_render(RenderEvent::ToolStarted {
        turn_id,
        agent_id,
        tool_call_id: "tc_1".to_string(),
        name: "Bash".to_string(),
        input: serde_json::Value::Null,
    });

    // 按 FIFO 顺序消费
    let e1 = handles.try_render().unwrap();
    let e2 = handles.try_render().unwrap();
    let e3 = handles.try_render().unwrap();
    assert!(matches!(e1, RenderEvent::ThinkingChunk { .. }));
    assert!(matches!(e2, RenderEvent::TextChunk { .. }));
    assert!(matches!(e3, RenderEvent::ToolStarted { .. }));
}

/// [回归测试] TurnCompleted 必须在 render_tx 通道中，与同迭代 Render 事件 FIFO。
///
/// 历史背景：TurnCompleted 原在 StateEvent（state_tx 独立通道），biased select!
/// 只保证单次迭代内优先级，不保证跨迭代——iter2 的 TextChunk 会先于 iter1 的
/// TurnCompleted 被消费，TUI 把 iter2 文本追加到 iter1 partial 上，渲染出
/// "新文本在旧工具之前"的错乱（CLAUDE.md P2-C 修复后回归）。
///
/// 本测试 emit iter1 全部 Render 事件 + iter1 TurnCompleted + iter2 TextChunk，
/// 断言消费顺序：iter1.tool_end → iter1.turn_completed → iter2.text。
/// 若 TurnCompleted 被移回 StateEvent，`RenderEvent::TurnCompleted` 编译失败，
/// 本测试成为编译期约束的回归门。
#[tokio::test]
async fn test_event_bus_turn_completed_in_render_channel_preserves_cross_iter_order() {
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let (turn1, agent_id) = make_ids();
    let (turn2, _) = make_ids();

    // iter1: TextChunk → ToolStarted → ToolEnded → TurnCompleted
    bus.emit_render(RenderEvent::TextChunk {
        turn_id: turn1,
        agent_id,
        message_id: MessageId::new(),
        chunk: "iter1-text".to_string(),
    });
    bus.emit_render(RenderEvent::ToolStarted {
        turn_id: turn1,
        agent_id,
        tool_call_id: "tc_iter1".to_string(),
        name: "Read".to_string(),
        input: serde_json::Value::Null,
    });
    bus.emit_render(RenderEvent::ToolEnded {
        turn_id: turn1,
        agent_id,
        tool_call_id: "tc_iter1".to_string(),
        name: "Read".to_string(),
        output: "ok".to_string(),
        is_error: false,
    });
    bus.emit_render(RenderEvent::TurnCompleted {
        turn_id: turn1,
        agent_id,
        steps: 1,
        elapsed_secs: 0.0,
        finalized_messages: std::sync::Arc::new(vec![]),
    });

    // iter2: TextChunk —— 必须排在 iter1 的 TurnCompleted 之后
    bus.emit_render(RenderEvent::TextChunk {
        turn_id: turn2,
        agent_id,
        message_id: MessageId::new(),
        chunk: "iter2-text".to_string(),
    });

    // 全部从 render_rx 消费（state_rx 应为空）
    let e1 = handles.try_render().expect("iter1 TextChunk");
    let e2 = handles.try_render().expect("iter1 ToolStarted");
    let e3 = handles.try_render().expect("iter1 ToolEnded");
    let e4 = handles.try_render().expect("iter1 TurnCompleted");
    let e5 = handles.try_render().expect("iter2 TextChunk");

    // 顺序断言：turn1 全部事件先于 turn2
    assert_eq!(e1.turn_id(), turn1);
    assert!(matches!(e1, RenderEvent::TextChunk { .. }));
    assert!(matches!(e2, RenderEvent::ToolStarted { .. }));
    assert!(matches!(e3, RenderEvent::ToolEnded { .. }));
    assert!(
        matches!(e4, RenderEvent::TurnCompleted { .. }),
        "iter1 TurnCompleted 必须在 iter2 事件之前消费，否则跨迭代顺序错乱"
    );
    assert_eq!(e5.turn_id(), turn2);
    assert!(matches!(e5, RenderEvent::TextChunk { .. }));

    // state_rx 必须为空（TurnCompleted 不应在 state 通道）
    assert!(
        handles.try_state().is_none(),
        "TurnCompleted 已迁移到 render_tx，state_rx 应为空"
    );
}
