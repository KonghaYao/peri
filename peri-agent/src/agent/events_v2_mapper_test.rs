//! 从 events_v2_mapper.rs 分离的测试模块

use super::*;

use crate::agent::events_v2::{RenderEvent, StateEvent};
use crate::group::pipeline::AgentId;
use crate::session::turn::TurnId;

fn ids() -> (TurnId, AgentId) {
    (TurnId::new(), AgentId::new())
}

#[test]
fn test_text_chunk_maps() {
    let (turn_id, agent_id) = ids();
    let r = RenderEvent::TextChunk {
        turn_id,
        agent_id,
        chunk: "hello".to_string(),
    };
    let executor_event = render_event_to_executor(r).expect("TextChunk 应映射");
    match executor_event {
        ExecutorEvent::TextChunk { chunk, .. } => assert_eq!(chunk, "hello"),
        _ => panic!("应为 TextChunk"),
    }
}

#[test]
fn test_thinking_chunk_maps() {
    let (turn_id, agent_id) = ids();
    let r = RenderEvent::ThinkingChunk {
        turn_id,
        agent_id,
        chunk: "thinking".to_string(),
    };
    match render_event_to_executor(r).unwrap() {
        ExecutorEvent::AiReasoning {
            text,
            source_agent_id,
        } => {
            assert_eq!(text, "thinking");
            assert!(source_agent_id.is_none());
        }
        _ => panic!("应为 AiReasoning"),
    }
}

#[test]
fn test_tool_started_maps() {
    let (turn_id, agent_id) = ids();
    let r = RenderEvent::ToolStarted {
        turn_id,
        agent_id,
        tool_call_id: "tc_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!("test input"),
    };
    match render_event_to_executor(r).unwrap() {
        ExecutorEvent::ToolStart {
            tool_call_id,
            name,
            input,
            ..
        } => {
            assert_eq!(tool_call_id, "tc_1");
            assert_eq!(name, "Read");
            assert_eq!(input, serde_json::json!("test input"));
        }
        _ => panic!("应为 ToolStart"),
    }
}

#[test]
fn test_tool_ended_maps() {
    let (turn_id, agent_id) = ids();
    let r = RenderEvent::ToolEnded {
        turn_id,
        agent_id,
        tool_call_id: "tc_1".to_string(),
        name: "Read".to_string(),
        output: "rejected".to_string(),
        is_error: true,
    };
    match render_event_to_executor(r).unwrap() {
        ExecutorEvent::ToolEnd {
            tool_call_id,
            is_error,
            ..
        } => {
            assert_eq!(tool_call_id, "tc_1");
            assert!(is_error);
        }
        _ => panic!("应为 ToolEnd"),
    }
}

#[test]
fn test_render_event_tool_ended_carries_output() {
    // ToolEnded 携带非空 output → mapper_v2 透传后 ExecutorEvent::ToolEnd.output 非空
    let (turn_id, agent_id) = ids();
    let r = RenderEvent::ToolEnded {
        turn_id,
        agent_id,
        tool_call_id: "tc_out".to_string(),
        name: "Bash".to_string(),
        output: "hello world\nline2".to_string(),
        is_error: false,
    };
    match render_event_to_executor(r).expect("ToolEnded 应映射为 ToolEnd") {
        ExecutorEvent::ToolEnd {
            output,
            is_error,
            tool_call_id,
            name,
            ..
        } => {
            assert_eq!(tool_call_id, "tc_out");
            assert_eq!(name, "Bash");
            assert!(!is_error);
            assert_eq!(output, "hello world\nline2");
            assert!(!output.is_empty(), "output 透传后必须非空");
        }
        other => panic!("应为 ToolEnd，实际 {:?}", other),
    }
}

#[test]
fn test_budget_warning_maps() {
    let (turn_id, agent_id) = ids();
    let r = RenderEvent::BudgetWarning {
        turn_id,
        agent_id,
        used_tokens: 1000,
        total_tokens: 200000,
        percentage: 0.5,
    };
    match render_event_to_executor(r).unwrap() {
        ExecutorEvent::ContextWarning {
            used_tokens,
            total_tokens,
            ..
        } => {
            assert_eq!(used_tokens, 1000);
            assert_eq!(total_tokens, 200000);
        }
        _ => panic!("应为 ContextWarning"),
    }
}

#[test]
fn test_hitl_pending_filtered() {
    let (turn_id, agent_id) = ids();
    let r = RenderEvent::HitlPending {
        turn_id,
        agent_id,
        tool_call_id: "tc".to_string(),
        tool_name: "Bash".to_string(),
    };
    assert!(render_event_to_executor(r).is_none());
}

#[test]
fn test_render_event_turn_committed_carries_messages() {
    // TurnCompleted（在 Render 层）携带 finalized_messages → TurnCommitted.messages 全量透传
    let (turn_id, agent_id) = ids();
    let msgs = vec![
        crate::messages::BaseMessage::human(crate::messages::MessageContent::text(
            "hello".to_string(),
        )),
        crate::messages::BaseMessage::ai(crate::messages::MessageContent::text(
            "world".to_string(),
        )),
    ];
    let r = RenderEvent::TurnCompleted {
        turn_id,
        agent_id,
        steps: 3,
        elapsed_secs: 0.1,
        finalized_messages: std::sync::Arc::new(msgs.clone()),
    };
    match render_event_to_executor(r).expect("TurnCompleted 不应被丢弃") {
        ExecutorEvent::TurnCommitted { messages, steps } => {
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].content(), "hello");
            assert_eq!(messages[1].content(), "world");
            assert_eq!(steps, 3);
        }
        other => panic!("应为 TurnCommitted，实际 {:?}", other),
    }
}

#[test]
fn test_state_event_snapshot_maps_to_meta() {
    // v2 StateSnapshot 应映射为 ExecutorEvent::StateSnapshotMeta，且字段完整透传
    let (turn_id, agent_id) = ids();
    let s = StateEvent::StateSnapshot {
        turn_id,
        agent_id,
        message_count: 9,
        total_tokens: 4321,
        current_step: 4,
        consecutive_failures: 2,
        budget_pct: Some(0.66),
        context_total_tokens: Some(150_000),
    };
    let exec_ev = state_event_to_executor(s).expect("StateSnapshot 不应被丢弃");
    match exec_ev {
        ExecutorEvent::StateSnapshotMeta {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
        } => {
            assert_eq!(message_count, 9);
            assert_eq!(total_tokens, 4321);
            assert_eq!(current_step, 4);
            assert_eq!(consecutive_failures, 2);
            assert_eq!(budget_pct, Some(0.66));
            assert_eq!(context_total_tokens, Some(150_000));
        }
        other => panic!("应为 StateSnapshotMeta，实际 {:?}", other),
    }
}

#[test]
fn test_state_event_snapshot_meta_none_budget_preserved() {
    // budget_pct=None / context_total_tokens=None 应原样透传（无 context_budget 场景）
    let (turn_id, agent_id) = ids();
    let s = StateEvent::StateSnapshot {
        turn_id,
        agent_id,
        message_count: 0,
        total_tokens: 0,
        current_step: 0,
        consecutive_failures: 0,
        budget_pct: None,
        context_total_tokens: None,
    };
    match state_event_to_executor(s).unwrap() {
        ExecutorEvent::StateSnapshotMeta {
            budget_pct,
            context_total_tokens,
            ..
        } => {
            assert!(budget_pct.is_none());
            assert!(context_total_tokens.is_none());
        }
        _ => panic!("应为 StateSnapshotMeta"),
    }
}

#[test]
fn test_observe_llm_call_end_maps_with_usage() {
    let (turn_id, agent_id) = ids();
    let o = ObserveEvent::LlmCallEnd {
        turn_id,
        agent_id,
        step: 7,
        model: "claude-sonnet-4".to_string(),
        output: "test output".to_string(),
        input_tokens: 500,
        output_tokens: 200,
        cache_creation_input_tokens: 30,
        cache_read_input_tokens: 400,
        request_id: Some("req-abc".to_string()),
    };
    match observe_event_to_executor(o).unwrap() {
        ExecutorEvent::LlmCallEnd {
            usage,
            model,
            step,
            request_id,
            ..
        } => {
            let u = usage.expect("应有 usage");
            assert_eq!(u.input_tokens, 500);
            assert_eq!(u.output_tokens, 200);
            assert_eq!(
                u.cache_creation_input_tokens,
                Some(30),
                "cache_creation 必须从 v2 透传到 v1（v2 重做回归）"
            );
            assert_eq!(
                u.cache_read_input_tokens,
                Some(400),
                "cache_read 必须从 v2 透传到 v1（v2 重做回归）"
            );
            assert_eq!(model, "claude-sonnet-4");
            assert_eq!(step, 7, "step 字段应从 v2 透传到 v1（非 0）");
            assert_eq!(
                request_id.as_deref(),
                Some("req-abc"),
                "request_id 必须从 v2 透传到 v1，不得随 usage 迁移丢失"
            );
        }
        _ => panic!("应为 LlmCallEnd"),
    }
}

#[test]
fn test_observe_llm_call_end_maps_with_output() {
    // v2 LlmCallEnd.output 非空 → mapper_v2 透传到 ExecutorEvent::LlmCallEnd.output 非空
    let (turn_id, agent_id) = ids();
    let o = ObserveEvent::LlmCallEnd {
        turn_id,
        agent_id,
        step: 3,
        model: "claude-sonnet-4".to_string(),
        output: "final answer text".to_string(),
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        request_id: None,
    };
    match observe_event_to_executor(o).expect("LlmCallEnd 应映射") {
        ExecutorEvent::LlmCallEnd { output, step, .. } => {
            assert_eq!(output, "final answer text");
            assert_eq!(step, 3);
        }
        _ => panic!("应为 LlmCallEnd"),
    }
}

#[test]
fn test_observe_llm_call_start_maps_with_messages_tools() {
    // v2 LlmCallStart 携带 messages + tools → mapper_v2 不再返回 None
    let (turn_id, agent_id) = ids();
    let s = ObserveEvent::LlmCallStart {
        turn_id,
        agent_id,
        step: 2,
        messages: std::sync::Arc::new(vec![]),
        tools: vec![],
    };
    let mapped = observe_event_to_executor(s).expect("LlmCallStart 应映射为 Some");
    match mapped {
        ExecutorEvent::LlmCallStart {
            step,
            messages,
            tools,
        } => {
            assert_eq!(step, 2);
            assert!(messages.is_empty());
            assert!(tools.is_empty());
        }
        _ => panic!("应为 LlmCallStart"),
    }
}

#[test]
fn test_observe_messages_compacted_maps() {
    let (turn_id, agent_id) = ids();
    let o = ObserveEvent::MessagesCompacted {
        turn_id,
        agent_id,
        before_count: 100,
        after_count: 30,
        summary: "compressed".to_string(),
        messages: vec![],
        files: vec![],
        skills: vec![],
        re_inject_count: 0,
        strategy: crate::agent::events::CompactStrategy::Micro,
        affected_count: 0,
        estimated_tokens_saved: 0,
        estimated_tokens_before: 0,
        estimated_tokens_after: 0,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
        full_escalation_reason: None,
        cache_hit_rate_before: 0.0,
        outcome: crate::agent::compact_v2::CompactOutcome::MicroApplied,
    };
    match observe_event_to_executor(o).unwrap() {
        ExecutorEvent::CompactCompleted {
            summary,
            micro_cleared,
            ..
        } => {
            assert_eq!(summary, "compressed");
            assert_eq!(micro_cleared, 70);
        }
        _ => panic!("应为 CompactCompleted"),
    }
}

#[test]
fn test_observe_subagent_lifecycle_maps() {
    let (turn_id, agent_id) = ids();
    let child = AgentId::new();

    let start = ObserveEvent::SubagentStart {
        turn_id,
        agent_id,
        child_agent_id: child,
        agent_name: "researcher".to_string(),
        is_background: false,
    };
    match observe_event_to_executor(start).unwrap() {
        ExecutorEvent::SubagentStarted {
            agent_name,
            is_background,
            ..
        } => {
            assert_eq!(agent_name, "researcher");
            assert!(!is_background);
        }
        _ => panic!("应为 SubagentStarted"),
    }

    let stop = ObserveEvent::SubagentStop {
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
        child_agent_id: child,
        agent_name: "researcher".to_string(),
        result: "done".to_string(),
        is_error: false,
    };
    match observe_event_to_executor(stop).unwrap() {
        ExecutorEvent::SubagentStopped {
            agent_name, result, ..
        } => {
            assert_eq!(agent_name, "researcher");
            assert_eq!(result, "done");
        }
        _ => panic!("应为 SubagentStopped"),
    }
}
