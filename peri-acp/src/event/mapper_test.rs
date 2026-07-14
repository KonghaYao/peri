use peri_agent::agent::events::{
    BackgroundTaskResult, CompactFileInfo, CompactStrategy, CompactTrigger, ExecutorEvent,
    TodoEntry, TodoStatus,
};
use peri_agent::llm::types::{StopReason, TokenUsage};
use peri_agent::messages::{BaseMessage, MessageId};
use peri_agent::tools::ToolDefinition;

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
            request_id: Some("req-123".to_string()),
        }),
        stop_reason: Some(StopReason::EndTurn),
    };

    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1, "应产出 1 个 MappedEvent");

    let m = &mapped[0];
    assert!(!m.forward_to_tui, "LlmCallEnd 不应转发到 TUI");
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
            request_id: None,
        }),
        stop_reason: None,
    };

    let mapped = map_event(&event, 128_000);
    assert_eq!(mapped.len(), 1);
    assert!(!mapped[0].forward_to_tui);

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
    };
    let mapped = map_event(&event, 200_000);
    assert!(
        mapped.is_empty()
            || mapped
                .iter()
                .all(|m| m.updates.is_empty() && !m.forward_to_tui),
        "LlmCallEnd usage=None 应被过滤"
    );
}

#[test]
fn test_context_warning_is_tui_only() {
    let event = ExecutorEvent::ContextWarning {
        used_tokens: 150_000,
        total_tokens: 200_000,
        percentage: 75.0,
    };
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1);
    assert!(mapped[0].forward_to_tui, "ContextWarning 应转发到 TUI");
    assert!(
        mapped[0].updates.is_empty(),
        "ContextWarning 不应产生 SessionUpdate"
    );
}

#[test]
fn test_llm_retrying_is_tui_only() {
    let event = ExecutorEvent::LlmRetrying {
        attempt: 2,
        max_attempts: 3,
        delay_ms: 1000,
        error: "timeout".to_string(),
    };
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1);
    assert!(mapped[0].forward_to_tui, "LlmRetrying 应转发到 TUI");
    assert!(
        mapped[0].updates.is_empty(),
        "LlmRetrying 不应产生 SessionUpdate"
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
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1);
    assert!(!mapped[0].forward_to_tui, "ToolEnd 不应转发到 TUI");
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
fn test_stop_reason_display_roundtrip() {
    for (reason, expected) in [
        (StopReason::EndTurn, "end_turn"),
        (StopReason::ToolUse, "tool_use"),
        (StopReason::MaxTokens, "max_tokens"),
        (StopReason::Other("custom".to_string()), "custom"),
    ] {
        let s = reason.to_string();
        assert_eq!(&s, expected, "Display 不匹配");
        assert_eq!(StopReason::from_display(&s), reason, "from_display 不匹配");
    }
}

// ── Category ①: SessionUpdate 变体 ──────────────────────────────────────────

#[test]
fn test_ai_reasoning_maps_to_session_update() {
    // AiReasoning → AgentThoughtChunk SessionUpdate，forward_to_tui=false
    let event = ExecutorEvent::AiReasoning {
        text: "let me think...".to_string(),
        source_agent_id: None,
    };
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1, "应产出 1 个 MappedEvent");
    assert!(!mapped[0].forward_to_tui, "AiReasoning 不应转发到 TUI");
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
        text: "subagent thinking...".to_string(),
        source_agent_id: Some("sa-1".to_string()),
    };
    let mapped = map_event(&event, 200_000);
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
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1);
    assert!(!mapped[0].forward_to_tui, "TextChunk 不应转发到 TUI");
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
    let mapped = map_event(&event, 200_000);
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
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1);
    assert!(!mapped[0].forward_to_tui, "ToolStart 不应转发到 TUI");
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
        let mapped = map_event(&event, 200_000);
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
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1);
    assert!(!mapped[0].forward_to_tui, "TodoUpdate 不应转发到 TUI");
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
    let mapped = map_event(&event, 200_000);
    assert_eq!(mapped.len(), 1);
    match &mapped[0].updates[0] {
        SessionUpdate::Plan(plan) => {
            assert!(plan.entries.is_empty(), "空 TodoUpdate 应产出空 Plan");
        }
        other => panic!("预期 Plan，实际: {:?}", other),
    }
}

// ── Category ③: TUI-only 变体 ──────────────────────────────────────────────
// 所有 TUI-only 变体应满足: forward_to_tui=true, updates 为空

fn assert_tui_only(event: &ExecutorEvent, label: &str) {
    let mapped = map_event(event, 200_000);
    assert_eq!(mapped.len(), 1, "{} 应产出 1 个 MappedEvent", label);
    assert!(mapped[0].forward_to_tui, "{} 应转发到 TUI", label);
    assert!(
        mapped[0].updates.is_empty(),
        "{} 不应产生 SessionUpdate",
        label
    );
}

#[test]
fn test_state_snapshot_is_tui_only() {
    assert_tui_only(&ExecutorEvent::StateSnapshot(vec![]), "StateSnapshot");
}

#[test]
fn test_state_snapshot_meta_is_tui_only() {
    // v2 路径的轻量级元数据快照应走 TUI-only 路由（不生成 SessionUpdate）
    assert_tui_only(
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
fn test_state_snapshot_meta_maps_to_acp_dto() {
    // 验证元数据字段完整透传到 AcpEvent DTO（不丢字段）
    let event = ExecutorEvent::StateSnapshotMeta {
        message_count: 7,
        total_tokens: 1234,
        current_step: 3,
        consecutive_failures: 1,
        budget_pct: Some(0.55),
        context_total_tokens: Some(100_000),
    };
    let acp =
        crate::event::executor_event_to_acp(&event).expect("StateSnapshotMeta 应映射到 AcpEvent");
    match acp {
        crate::event::AcpEvent::StateSnapshotMeta {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
        } => {
            assert_eq!(message_count, 7);
            assert_eq!(total_tokens, 1234);
            assert_eq!(current_step, 3);
            assert_eq!(consecutive_failures, 1);
            assert_eq!(budget_pct, Some(0.55));
            assert_eq!(context_total_tokens, Some(100_000));
        }
        other => panic!("应为 StateSnapshotMeta，实际 {:?}", other),
    }
}

#[test]
fn test_subagent_started_is_tui_only() {
    assert_tui_only(
        &ExecutorEvent::SubagentStarted {
            agent_name: "sub-agent".to_string(),
            instance_id: "inst-001".to_string(),
            is_background: false,
        },
        "SubagentStarted",
    );
}

#[test]
fn test_subagent_stopped_is_tui_only() {
    assert_tui_only(
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
fn test_compact_started_is_tui_only() {
    assert_tui_only(
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
fn test_compact_completed_is_tui_only() {
    assert_tui_only(
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
        },
        "CompactCompleted",
    );
}

#[test]
fn test_compact_error_is_tui_only() {
    assert_tui_only(
        &ExecutorEvent::CompactError {
            message: "compact failed".to_string(),
        },
        "CompactError",
    );
}

#[test]
fn test_background_task_completed_is_tui_only() {
    assert_tui_only(
        &ExecutorEvent::BackgroundTaskCompleted(BackgroundTaskResult {
            task_id: "bg-001".to_string(),
            agent_name: "bg-agent".to_string(),
            prompt_summary: "do stuff".to_string(),
            success: true,
            output: "ok".to_string(),
            tool_calls_count: 3,
            duration_ms: 5000,
            child_thread_id: None,
        }),
        "BackgroundTaskCompleted",
    );
}

#[test]
fn test_lsp_diagnostics_is_tui_only() {
    assert_tui_only(
        &ExecutorEvent::LspDiagnostics {
            errors: 2,
            warnings: 5,
            files_with_errors: 3,
        },
        "LspDiagnostics",
    );
}

#[test]
fn test_agent_execution_failed_is_tui_only() {
    assert_tui_only(
        &ExecutorEvent::AgentExecutionFailed {
            message: "agent crashed".to_string(),
        },
        "AgentExecutionFailed",
    );
}

// ── Filtered 变体 ─────────────────────────────────────────────────────────
// 所有 filtered 变体应满足: forward_to_tui=false, updates 为空

fn assert_filtered(event: &ExecutorEvent, label: &str) {
    let mapped = map_event(event, 200_000);
    assert_eq!(mapped.len(), 1, "{} 应产出 1 个 MappedEvent", label);
    assert!(!mapped[0].forward_to_tui, "{} 不应转发到 TUI", label);
    assert!(
        mapped[0].updates.is_empty(),
        "{} 不应产生 SessionUpdate",
        label
    );
}

#[test]
fn test_message_added_produces_user_message_chunk() {
    let result = map_event(
        &ExecutorEvent::MessageAdded(BaseMessage::human("bg result text")),
        200000,
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
fn test_llm_call_start_produces_no_output() {
    assert_filtered(
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
