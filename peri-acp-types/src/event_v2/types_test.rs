//! 事件身份与序列化契约。

use super::*;

use crate::identity::AgentId;
use crate::messages::MessageId;
use crate::session::TurnId;

// ─── 构造辅助 ──────────────────────────────────────────────────────────

fn make_ids() -> (TurnId, AgentId) {
    (TurnId::new(), AgentId::new())
}

// ─── TurnErrorReason 测试 ────────────────────────────────────────────────

#[test]
fn test_turn_error_reason_display() {
    assert_eq!(TurnErrorReason::Interrupted.to_string(), "interrupted");
    assert_eq!(TurnErrorReason::Timeout.to_string(), "timeout");
    assert_eq!(TurnErrorReason::LlmFailure.to_string(), "llm_failure");
    assert_eq!(TurnErrorReason::ToolFailure.to_string(), "tool_failure");
    assert_eq!(TurnErrorReason::RateLimit.to_string(), "rate_limit");
    assert_eq!(TurnErrorReason::MaxIterations.to_string(), "max_iterations");
}

#[test]
fn test_turn_error_reason_serde_roundtrip() {
    let reasons = [
        TurnErrorReason::Interrupted,
        TurnErrorReason::Timeout,
        TurnErrorReason::LlmFailure,
        TurnErrorReason::ToolFailure,
        TurnErrorReason::RateLimit,
        TurnErrorReason::MaxIterations,
    ];
    for reason in &reasons {
        let json = serde_json::to_string(reason).unwrap();
        let back: TurnErrorReason = serde_json::from_str(&json).unwrap();
        assert_eq!(*reason, back);
    }
}

// ─── RenderEvent 测试 ──────────────────────────────────────────────────

#[test]
fn test_render_event_text_chunk_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "hello".to_string(),
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_render_event_thinking_chunk_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::ThinkingChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "thinking...".to_string(),
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_render_event_tool_started_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::ToolStarted {
        turn_id,
        agent_id,
        tool_call_id: "tc_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::Value::Null,
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_render_event_tool_ended_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::ToolEnded {
        turn_id,
        agent_id,
        tool_call_id: "tc_1".to_string(),
        name: "Read".to_string(),
        output: "file contents".to_string(),
        is_error: false,
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_render_event_tool_ended_carries_output() {
    // ToolEnded 必须携带非空 output，经 共享协议映射 透传后 TUI 才能拿到工具结果
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::ToolEnded {
        turn_id,
        agent_id,
        tool_call_id: "tc_out".to_string(),
        name: "Bash".to_string(),
        output: "command output here".to_string(),
        is_error: false,
    };
    // 通过模式匹配断言 output 字段存在且非空
    match event {
        RenderEvent::ToolEnded { ref output, .. } => {
            assert!(!output.is_empty(), "output 应为非空字符串");
            assert_eq!(output, "command output here");
        }
        _ => panic!("应为 ToolEnded"),
    }
}

#[test]
fn test_render_event_budget_warning_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::BudgetWarning {
        turn_id,
        agent_id,
        used_tokens: 1000,
        total_tokens: 200000,
        percentage: 0.5,
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_render_event_hitl_pending_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::HitlPending {
        turn_id,
        agent_id,
        tool_call_id: "tc_2".to_string(),
        tool_name: "Bash".to_string(),
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

// ─── StateEvent 测试 ───────────────────────────────────────────────────

#[test]
fn test_render_event_turn_completed_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::TurnCompleted {
        turn_id,
        agent_id,
        steps: 5,
        elapsed_secs: 3.2,
        finalized_messages: std::sync::Arc::new(vec![]),
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_state_event_snapshot_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = StateEvent::StateSnapshot {
        turn_id,
        agent_id,
        message_count: 42,
        total_tokens: 10000,
        current_step: 3,
        consecutive_failures: 0,
        budget_pct: Some(0.45),
        context_total_tokens: Some(200_000),
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

// ─── ObserveEvent 测试 ──────────────────────────────────────────────────

#[test]
fn test_observe_event_llm_call_start_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = ObserveEvent::LlmCallStart {
        turn_id,
        agent_id,
        step: 1,
        messages: std::sync::Arc::new(vec![]),
        tools: vec![],
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_observe_event_llm_call_end_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = ObserveEvent::LlmCallEnd {
        turn_id,
        agent_id,
        step: 1,
        model: "claude-sonnet-4-20250514".to_string(),
        output: "test output".to_string(),
        input_tokens: 500,
        output_tokens: 200,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: None,
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_observe_event_compact_started_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = ObserveEvent::CompactStarted {
        turn_id,
        agent_id,
        step: 3,
        strategy: crate::event::CompactStrategy::Micro,
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_observe_event_messages_compacted_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = ObserveEvent::MessagesCompacted {
        turn_id,
        agent_id,
        before_count: 100,
        after_count: 30,
        summary: "compact done".to_string(),
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
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_observe_event_turn_error_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let event = ObserveEvent::TurnError {
        turn_id,
        agent_id,
        reason: TurnErrorReason::MaxIterations,
        message: "hit limit".to_string(),
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_observe_event_subagent_start_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let child_id = AgentId::new();
    let event = ObserveEvent::SubagentStart {
        turn_id,
        agent_id,
        child_agent_id: child_id,
        agent_name: "researcher".to_string(),
        is_background: true,
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_observe_event_subagent_stop_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let child_id = AgentId::new();
    let event = ObserveEvent::SubagentStop {
        turn_id,
        agent_id,
        child_agent_id: child_id,
        agent_name: "researcher".to_string(),
        result: "done".to_string(),
        is_error: false,
    };
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

// ─── Event（统一包装）测试 ─────────────────────────────────────────────

#[test]
fn test_event_unified_turn_id_extraction() {
    let (turn_id, agent_id) = make_ids();
    let render = Event::Render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: MessageId::new(),
        chunk: "hi".to_string(),
    });
    assert_eq!(render.turn_id(), turn_id);
    assert_eq!(render.agent_id(), agent_id);
}

#[test]
fn test_event_unified_state_extraction() {
    let (turn_id, agent_id) = make_ids();
    let state = Event::State(StateEvent::StateSnapshot {
        turn_id,
        agent_id,
        message_count: 1,
        total_tokens: 100,
        current_step: 1,
        consecutive_failures: 0,
        budget_pct: None,
        context_total_tokens: None,
    });
    assert_eq!(state.turn_id(), turn_id);
    assert_eq!(state.agent_id(), agent_id);
}

#[test]
fn test_event_unified_render_turn_completed_extraction() {
    // TurnCompleted 在 Render 层，验证 Event::Render 包装后 id 提取正确
    let (turn_id, agent_id) = make_ids();
    let event = Event::Render(RenderEvent::TurnCompleted {
        turn_id,
        agent_id,
        steps: 1,
        elapsed_secs: 0.5,
        finalized_messages: std::sync::Arc::new(vec![]),
    });
    assert_eq!(event.turn_id(), turn_id);
    assert_eq!(event.agent_id(), agent_id);
}

#[test]
fn test_event_unified_observe_extraction() {
    let (turn_id, agent_id) = make_ids();
    let observe = Event::Observe(ObserveEvent::LlmCallStart {
        turn_id,
        agent_id,
        step: 0,
        messages: std::sync::Arc::new(vec![]),
        tools: vec![],
    });
    assert_eq!(observe.turn_id(), turn_id);
    assert_eq!(observe.agent_id(), agent_id);
}

// ─── 序列化测试 ─────────────────────────────────────────────────────────

#[test]
fn test_render_event_serde_roundtrip() {
    let (turn_id, agent_id) = make_ids();
    let event = RenderEvent::HitlPending {
        turn_id,
        agent_id,
        tool_call_id: "tc_1".to_string(),
        tool_name: "Bash".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: RenderEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(event.turn_id(), back.turn_id());
    assert_eq!(event.agent_id(), back.agent_id());
}

#[test]
fn test_observe_event_serde_roundtrip() {
    let (turn_id, agent_id) = make_ids();
    let event = ObserveEvent::TurnError {
        turn_id,
        agent_id,
        reason: TurnErrorReason::RateLimit,
        message: "429".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: ObserveEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        back,
        ObserveEvent::TurnError {
            reason: TurnErrorReason::RateLimit,
            ..
        }
    ));
}

#[test]
fn test_observe_event_compact_started_serde_roundtrip() {
    let (turn_id, agent_id) = make_ids();
    let event = ObserveEvent::CompactStarted {
        turn_id,
        agent_id,
        step: 7,
        strategy: crate::event::CompactStrategy::Micro,
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: ObserveEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, ObserveEvent::CompactStarted { step: 7, .. }));
}

/// C2/C3 事件契约：SubagentStart 序列化/反序列化 round-trip，全部字段全等
/// （生产 emit 依赖此契约：bridge 消费的事件必须字段完整、id 可反解）。
#[test]
fn test_observe_event_subagent_start_serde_roundtrip() {
    let (turn_id, agent_id) = make_ids();
    // child_agent_id 使用可解析的 UUID v7（身份键统一后 = child_thread_id）
    let child_agent_id = AgentId::from_uuid(uuid::Uuid::now_v7());
    let event = ObserveEvent::SubagentStart {
        turn_id,
        agent_id,
        child_agent_id,
        agent_name: "code-reviewer".to_string(),
        is_background: true,
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: ObserveEvent = serde_json::from_str(&json).unwrap();
    match back {
        ObserveEvent::SubagentStart {
            turn_id: t,
            agent_id: a,
            child_agent_id: c,
            agent_name,
            is_background,
        } => {
            assert_eq!(t, turn_id);
            assert_eq!(a, agent_id);
            assert_eq!(c, child_agent_id);
            assert_eq!(agent_name, "code-reviewer");
            assert!(is_background);
            // 身份契约：child_agent_id 字符串形式即 child_thread_id（instance_id）
            assert_eq!(
                c.as_uuid().to_string(),
                child_agent_id.as_uuid().to_string()
            );
        }
        other => panic!("应为 SubagentStart，实际 {:?}", other),
    }
}

/// C2/C3 事件契约：SubagentStop 序列化/反序列化 round-trip，全部字段全等
#[test]
fn test_observe_event_subagent_stop_serde_roundtrip() {
    let (turn_id, agent_id) = make_ids();
    let child_agent_id = AgentId::from_uuid(uuid::Uuid::now_v7());
    let event = ObserveEvent::SubagentStop {
        turn_id,
        agent_id,
        child_agent_id,
        agent_name: "code-reviewer".to_string(),
        result: "found 3 issues".to_string(),
        is_error: false,
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: ObserveEvent = serde_json::from_str(&json).unwrap();
    match back {
        ObserveEvent::SubagentStop {
            turn_id: t,
            agent_id: a,
            child_agent_id: c,
            agent_name,
            result,
            is_error,
        } => {
            assert_eq!(t, turn_id);
            assert_eq!(a, agent_id);
            assert_eq!(c, child_agent_id);
            assert_eq!(agent_name, "code-reviewer");
            assert_eq!(result, "found 3 issues");
            assert!(!is_error);
        }
        other => panic!("应为 SubagentStop，实际 {:?}", other),
    }
}

#[test]
fn test_event_unified_serde_roundtrip() {
    let (turn_id, agent_id) = make_ids();
    let event = Event::Render(RenderEvent::BudgetWarning {
        turn_id,
        agent_id,
        used_tokens: 150000,
        total_tokens: 200000,
        percentage: 0.75,
    });
    let json = serde_json::to_string(&event).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(event.turn_id(), back.turn_id());
    assert_eq!(event.agent_id(), back.agent_id());
}
