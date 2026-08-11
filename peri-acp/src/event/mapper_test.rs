use peri_acp_types::event::{
    BackgroundTaskResult, CompactFileInfo, CompactStrategy, CompactTrigger, ExecutorEvent,
    TodoEntry, TodoStatus,
};
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::tools::ToolDefinition;
use peri_acp_types::PeriCaps;
use peri_model::{StopReason, TokenUsage};

use super::*;

#[test]
fn test_llm_call_end_maps_to_enriched_usage_update() {
    let event = ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "claude-sonnet-4-20250514".to_string(),
        output: "Hello".to_string(),
        usage: Some(TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(200),
        }),
        stop_reason: Some(StopReason::EndTurn),
        request_id: Some("req-123".to_string()),
    };
    let caps = PeriCaps {
        token_stats: true,
        ..Default::default()
    };
    let mapped = map_event(&event, 200_000, &caps);
    assert_eq!(mapped.len(), 1, "应产出 1 个 MappedEvent");

    let m = &mapped[0];
    assert_eq!(m.updates.len(), 1, "应包含 1 个 SessionUpdate");

    match &m.updates[0] {
        SessionUpdate::UsageUpdate(usage) => {
            assert_eq!(usage.used, 150);
            assert_eq!(usage.size, 200_000);
            let meta = usage.meta.as_ref().expect("_meta 应包含详细 usage");
            assert_eq!(meta.get("inputTokens").unwrap().as_u64(), Some(100));
            assert_eq!(meta.get("outputTokens").unwrap().as_u64(), Some(50));
            assert_eq!(meta.get("cacheCreationTokens").unwrap().as_u64(), Some(10));
            assert_eq!(meta.get("cacheReadTokens").unwrap().as_u64(), Some(200));
            assert_eq!(
                meta.get("model").unwrap().as_str(),
                Some("claude-sonnet-4-20250514")
            );
            assert_eq!(meta.get("stopReason").unwrap().as_str(), Some("end_turn"));
            assert_eq!(
                meta.get("requestId").unwrap().as_str(),
                Some("req-123"),
                "requestId 必须从 LlmCallEnd.request_id 透传到 meta，不得随 usage 迁移丢失"
            );
        }
        other => panic!("预期 UsageUpdate，实际: {:?}", other),
    }
}

#[test]
fn test_llm_call_end_no_optional_fields() {
    // 无缓存 token、无 stop_reason 时 _meta 不含可选字段
    let event = ExecutorEvent::LlmCallEnd {
        step: 2,
        model: "gpt-4o".to_string(),
        output: String::new(),
        usage: Some(TokenUsage {
            input_tokens: 200,
            output_tokens: 30,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }),
        stop_reason: None,
        request_id: None,
    };
    let caps = PeriCaps {
        token_stats: true,
        ..Default::default()
    };
    let mapped = map_event(&event, 128_000, &caps);
    assert_eq!(mapped.len(), 1);

    match &mapped[0].updates[0] {
        SessionUpdate::UsageUpdate(usage) => {
            assert_eq!(usage.used, 230);
            let meta = usage.meta.as_ref().unwrap();
            assert!(meta.get("cacheCreationTokens").is_none());
            assert!(meta.get("cacheReadTokens").is_none());
            assert!(meta.get("stopReason").is_none());
        }
        other => panic!("预期 UsageUpdate，实际: {:?}", other),
    }
}

#[test]
fn test_llm_call_end_no_usage_filtered() {
    let event = ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "test".to_string(),
        output: "ERROR".to_string(),
        usage: None,
        stop_reason: None,
        request_id: None,
    };
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert!(
        mapped.iter().all(|m| m.updates.is_empty()),
        "LlmCallEnd usage=None 应被过滤"
    );
}

// ── Non-Category-① 变体：wildcard 无 SessionUpdate ────────────────────────
// 所有非 Category ① 变体现在通过 wildcard 产生空 updates

fn assert_no_session_update(event: &ExecutorEvent, label: &str) {
    let mapped = map_event(event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1, "{} 应产出 1 个 MappedEvent", label);
    assert!(
        mapped[0].updates.is_empty(),
        "{} 不应产生 SessionUpdate",
        label
    );
}

#[test]
fn test_context_warning_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::ContextWarning {
            used_tokens: 150_000,
            total_tokens: 200_000,
            percentage: 75.0,
        },
        "ContextWarning",
    );
}

#[test]
fn test_llm_retrying_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::LlmRetrying {
            attempt: 2,
            max_attempts: 3,
            delay_ms: 1000,
            error: "timeout".to_string(),
        },
        "LlmRetrying",
    );
}

#[test]
fn test_tool_end_carries_title() {
    // ToolEnd 映射为 ToolCallUpdate 时必须携带 title（工具名）
    let event = ExecutorEvent::ToolEnd {
        message_id: MessageId::new(),
        tool_call_id: "tc-123".to_string(),
        name: "Bash".to_string(),
        output: "ok".to_string(),
        is_error: false,
        source_agent_id: None,
    };
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);

    match &mapped[0].updates[0] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.tool_call_id.0.as_ref(), "tc-123");
            // title 必须携带工具名，不能为空
            let title = update.fields.title.as_deref().unwrap_or("");
            assert_eq!(title, "Bash", "ToolCallUpdate.title 应为工具名");
        }
        other => panic!("预期 ToolCallUpdate，实际: {:?}", other),
    }
}

#[test]
fn test_stop_reason_wire_format() {
    // legacy wire format：与历史 StopReason Display 一致。
    // peri_model::StopReason 无 Display，经 mapper 本地 helper 显式映射。
    for (reason, expected) in [
        (StopReason::EndTurn, "end_turn"),
        (StopReason::ToolUse, "tool_use"),
        (StopReason::MaxTokens, "max_tokens"),
        (
            StopReason::Other {
                value: "custom".to_string(),
            },
            "custom",
        ),
    ] {
        assert_eq!(stop_reason_wire(&reason), expected, "wire format 不匹配");
    }
}

// ── Category ①: SessionUpdate 变体 ──────────────────────────────────────────

#[test]
fn test_ai_reasoning_maps_to_session_update() {
    // AiReasoning → AgentThoughtChunk SessionUpdate
    let event = ExecutorEvent::AiReasoning {
        message_id: peri_acp_types::messages::MessageId::new(),
        text: "let me think...".to_string(),
        source_agent_id: None,
    };
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1, "应产出 1 个 MappedEvent");
    assert_eq!(mapped[0].updates.len(), 1, "应包含 1 个 SessionUpdate");
    assert!(
        mapped[0].source_agent_id.is_none(),
        "主 agent reasoning 无 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::AgentThoughtChunk(chunk) => {
            // 验证 ContentChunk 内含 Text ContentBlock
            match &chunk.content {
                ContentBlock::Text(tc) => {
                    assert_eq!(tc.text, "let me think...");
                }
                other => panic!("预期 Text ContentBlock，实际: {:?}", other),
            }
        }
        other => panic!("预期 AgentThoughtChunk，实际: {:?}", other),
    }
}

#[test]
fn test_ai_reasoning_with_source_agent_id_forwards_to_notifier() {
    // SubAgent reasoning → 应携带 source_agent_id，使 TUI notifier
    // 的 agent_thought_chunk handler 正确路由到 SubAgentGroup
    let event = ExecutorEvent::AiReasoning {
        message_id: peri_acp_types::messages::MessageId::new(),
        text: "subagent thinking...".to_string(),
        source_agent_id: Some("sa-1".to_string()),
    };
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(
        mapped[0].source_agent_id.as_deref(),
        Some("sa-1"),
        "SubAgent reasoning 应携带 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
            ContentBlock::Text(tc) => assert_eq!(tc.text, "subagent thinking..."),
            other => panic!("预期 Text，实际: {other:?}"),
        },
        other => panic!("预期 AgentThoughtChunk，实际: {other:?}"),
    }
}

#[test]
fn test_text_chunk_maps_to_session_update_with_source() {
    // TextChunk → AgentMessageChunk，携带 source_agent_id
    let event = ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: "Hello world".to_string(),
        source_agent_id: Some("sub-agent-1".to_string()),
    };
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);
    assert_eq!(
        mapped[0].source_agent_id.as_deref(),
        Some("sub-agent-1"),
        "应携带 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(tc) => {
                assert_eq!(tc.text, "Hello world");
            }
            other => panic!("预期 Text ContentBlock，实际: {:?}", other),
        },
        other => panic!("预期 AgentMessageChunk，实际: {:?}", other),
    }
}

#[test]
fn test_text_chunk_without_source_agent_id() {
    // TextChunk 无 source_agent_id 时 source_agent_id 为 None
    let event = ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: "main text".to_string(),
        source_agent_id: None,
    };
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert!(mapped[0].source_agent_id.is_none());
}

#[test]
fn test_tool_start_maps_to_session_update_with_tool_info() {
    // ToolStart → ToolCall SessionUpdate，携带 tool_call_id/name/kind/status/raw_input
    let event = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        tool_call_id: "tc-456".to_string(),
        name: "Bash".to_string(),
        input: serde_json::json!({"command": "ls -la"}),
        source_agent_id: Some("sub-agent-2".to_string()),
    };
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);
    assert_eq!(
        mapped[0].source_agent_id.as_deref(),
        Some("sub-agent-2"),
        "应携带 source_agent_id"
    );
    match &mapped[0].updates[0] {
        SessionUpdate::ToolCall(tc) => {
            assert_eq!(tc.tool_call_id.0.as_ref(), "tc-456");
            assert_eq!(tc.title, "Bash");
            assert_eq!(tc.kind, ToolKind::Execute, "Bash 应推断为 Execute");
            assert_eq!(tc.status, ToolCallStatus::InProgress);
            assert!(tc.raw_input.is_some(), "raw_input 应存在");
        }
        other => panic!("预期 ToolCall，实际: {:?}", other),
    }
}

#[test]
fn test_tool_start_infer_tool_kind_variants() {
    // 验证 infer_tool_kind 对不同工具名的推断结果
    let cases = [
        ("Read", ToolKind::Read),
        ("Write", ToolKind::Edit),
        ("Edit", ToolKind::Edit),
        ("folder_operations", ToolKind::Edit),
        ("Bash", ToolKind::Execute),
        ("Grep", ToolKind::Search),
        ("Glob", ToolKind::Search),
        ("WebFetch", ToolKind::Fetch),
        ("WebSearch", ToolKind::Fetch),
        ("mcp__server__tool", ToolKind::Other),
    ];
    for (name, expected_kind) in cases {
        let event = ExecutorEvent::ToolStart {
            message_id: MessageId::new(),
            tool_call_id: "tc-x".to_string(),
            name: name.to_string(),
            input: serde_json::Value::Null,
            source_agent_id: None,
        };
        let mapped = map_event(&event, 200_000, &PeriCaps::default());
        match &mapped[0].updates[0] {
            SessionUpdate::ToolCall(tc) => {
                assert_eq!(
                    tc.kind, expected_kind,
                    "工具名 {} 的 kind 应为 {:?}",
                    name, expected_kind
                );
            }
            other => panic!("{} 预期 ToolCall，实际: {:?}", name, other),
        }
    }
}

#[test]
fn test_todo_update_maps_to_session_update() {
    // TodoUpdate → Plan SessionUpdate，条目状态正确映射
    let entries = vec![
        TodoEntry {
            content: "实现功能 A".to_string(),
            active_form: Some("正在实现功能 A".to_string()),
            status: TodoStatus::InProgress,
        },
        TodoEntry {
            content: "测试功能 B".to_string(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoEntry {
            content: "完成功能 C".to_string(),
            active_form: None,
            status: TodoStatus::Completed,
        },
    ];
    let event = ExecutorEvent::TodoUpdate(entries);
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].updates.len(), 1);
    match &mapped[0].updates[0] {
        SessionUpdate::Plan(plan) => {
            assert_eq!(plan.entries.len(), 3, "Plan 应包含 3 个条目");
            assert_eq!(plan.entries[0].content, "实现功能 A");
            assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
            assert_eq!(plan.entries[1].status, PlanEntryStatus::Pending);
            assert_eq!(plan.entries[2].status, PlanEntryStatus::Completed);
            // 所有条目优先级为 Medium（mapper 中硬编码）
            for entry in &plan.entries {
                assert_eq!(entry.priority, PlanEntryPriority::Medium);
            }
        }
        other => panic!("预期 Plan，实际: {:?}", other),
    }
}

#[test]
fn test_todo_update_empty_entries() {
    // 空 TodoUpdate → 空 Plan（条目数为 0）
    let event = ExecutorEvent::TodoUpdate(vec![]);
    let mapped = map_event(&event, 200_000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1);
    match &mapped[0].updates[0] {
        SessionUpdate::Plan(plan) => {
            assert!(plan.entries.is_empty(), "空 TodoUpdate 应产出空 Plan");
        }
        other => panic!("预期 Plan，实际: {:?}", other),
    }
}

#[test]
fn test_state_snapshot_no_session_update() {
    assert_no_session_update(&ExecutorEvent::StateSnapshot(vec![]), "StateSnapshot");
}

#[test]
fn test_state_snapshot_meta_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::StateSnapshotMeta {
            message_count: 5,
            total_tokens: 0,
            current_step: 2,
            consecutive_failures: 0,
            budget_pct: Some(0.42),
            context_total_tokens: Some(200_000),
        },
        "StateSnapshotMeta",
    );
}

#[test]
fn test_subagent_started_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::SubagentStarted {
            agent_name: "sub-agent".to_string(),
            instance_id: "inst-001".to_string(),
            is_background: false,
        },
        "SubagentStarted",
    );
}

#[test]
fn test_subagent_stopped_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::SubagentStopped {
            agent_name: "sub-agent".to_string(),
            result: "done".to_string(),
            is_error: false,
            instance_id: "inst-001".to_string(),
        },
        "SubagentStopped",
    );
}

#[test]
fn test_compact_started_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::CompactStarted {
            turn_id: "turn_1".into(),
            agent_id: "agent_1".into(),
            step: 0,
            strategy: CompactStrategy::Smart,
            trigger: CompactTrigger::Auto,
        },
        "CompactStarted",
    );
}

#[test]
fn test_compact_completed_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::CompactCompleted {
            summary: "compressed".to_string(),
            files: vec![CompactFileInfo {
                path: "src/main.rs".to_string(),
                lines: 100,
            }],
            skills: vec!["skill-a".to_string()],
            micro_cleared: 0,
            messages: vec![],
            token_before: 0,
            token_after: 0,
            strategy: CompactStrategy::Smart,
            affected_count: 0,
            estimated_tokens_saved: 0,
            estimated_tokens_before: 0,
            estimated_tokens_after: 0,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: 0,
            full_escalation_reason: None,
            cache_hit_rate_before: 0.0,
            trigger: CompactTrigger::Auto,
            outcome: peri_acp_types::compact::CompactOutcome::FullApplied,
        },
        "CompactCompleted",
    );
}

#[test]
fn test_compact_error_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::CompactError {
            message: "compact failed".to_string(),
        },
        "CompactError",
    );
}

#[test]
fn test_rewind_error_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::RewindError {
            message: "rewind: 未找到目标消息 abc".to_string(),
        },
        "RewindError",
    );
}

#[test]
fn test_background_task_completed_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::BackgroundTaskCompleted(BackgroundTaskResult {
            task_id: "bg-001".to_string(),
            agent_name: "bg-agent".to_string(),
            prompt_summary: "do stuff".to_string(),
            success: true,
            output: "ok".to_string(),
            tool_calls_count: 3,
            duration_ms: 5000,
            child_thread_id: None,
            timed_out: false,
        }),
        "BackgroundTaskCompleted",
    );
}

#[test]
fn test_lsp_diagnostics_no_session_update() {
    assert_no_session_update(
        &ExecutorEvent::LspDiagnostics {
            errors: 2,
            warnings: 5,
            files_with_errors: 3,
        },
        "LspDiagnostics",
    );
}

#[test]
fn test_message_added_produces_user_message_chunk() {
    let result = map_event(
        &ExecutorEvent::MessageAdded(BaseMessage::human("bg result text")),
        200000,
        &PeriCaps::default(),
    );
    assert_eq!(result.len(), 1);
    assert!(
        !result[0].updates.is_empty(),
        "MessageAdded 应产生 SessionUpdate"
    );
    match &result[0].updates[0] {
        SessionUpdate::UserMessageChunk(_chunk) => {
            // UserMessageChunk 携带 ContentChunk，由 ACP SDK 序列化——不测试内部结构
        }
        other => panic!("应为 UserMessageChunk，实际: {:?}", other),
    }
}

#[test]
fn test_llm_call_start_no_output() {
    assert_no_session_update(
        &ExecutorEvent::LlmCallStart {
            step: 1,
            messages: std::sync::Arc::new(vec![BaseMessage::human("hello")]),
            tools: vec![ToolDefinition {
                name: "Bash".to_string(),
                description: "Run command".to_string(),
                parameters: serde_json::Value::Null,
            }],
        },
        "LlmCallStart",
    );
}
