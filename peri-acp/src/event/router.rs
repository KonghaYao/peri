//! Event router -- maps `ExecutorEvent` to `peri/unstable-event` protocol payloads.
//!
//! Each call to [`route`] returns an optional [`RoutingOutput`] containing:
//! - `event_name`: kebab-case string identifying the event
//! - `data`: JSON value carrying the event-specific payload
//!
//! Events that are internal to the agent (LLM retries, compact lifecycle, etc.)
//! return `None` and are discarded per §5.1 of the protocol design doc.
//!
//! §4.1 streaming events (TextChunk/AiReasoning/ToolStart/ToolEnd)
//! are now routed through standard ACP `session/update`, not `peri/unstable-event`.
//!
//! Reference: `docs/design/peri-acp-protocol.md` section 5 "Event Router".

use peri_acp_types::event_data::BudgetWarning;
use peri_acp_types::event_data::RewindMessage;
use peri_acp_types::event_data::RewindPreview;
use peri_agent::agent::events::ExecutorEvent;

use super::truncate::truncate_text;

/// Output of [`route`] -- an event name + its JSON data payload.
#[derive(Debug, Clone)]
pub struct RoutingOutput {
    /// kebab-case event name (e.g. `"turn-done"`).
    pub event_name: String,
    /// Serialized event data.
    pub data: serde_json::Value,
}

/// Map an [`ExecutorEvent`] to a `peri/unstable-event` routing output.
///
/// Returns `None` for events that should be discarded (§5.1):
/// - `LlmRetrying` -- internal retry behavior
/// - `LspDiagnostics` -- only relevant within tool execution context
/// - `CompactStarted` / `CompactCompleted` / `CompactError` -- transparent to user
/// - `LlmCallStart` / `LlmRequestPayload` -- observability-only
/// - `MessageAdded` -- incremental, not needed for TUI rendering
/// - `StateSnapshot` -- superseded by incremental streaming for rendering
/// - `TurnCommitted` -- superseded by streaming + TurnDone for rendering
/// - `StateSnapshotMeta` -- status-bar metadata, not a push event
/// - `LlmCallEnd` -- status-bar metadata (token counts), now delivered via
///   standard ACP `session/update` (usage_update tag), §C
/// - `BackgroundTaskCompleted` / `BgToolStep` -- handled via separate TUI polling
/// - `WorkflowProgress` -- handled via dedicated panel
/// - `AgentExecutionFailed` -- routed as `"turn-interrupted"` so the v2 state machine can exit Streaming
pub fn route(ev: &ExecutorEvent) -> Option<RoutingOutput> {
    match ev {
        // ── §4.3 Status events ───────────────────────────────────────────────
        ExecutorEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        } => {
            let threshold = if *percentage >= 0.85 { "0.85" } else { "0.70" };
            Some(RoutingOutput {
                event_name: "budget-warning".into(),
                data: serde_json::to_value(&BudgetWarning {
                    used: *used_tokens,
                    limit: *total_tokens,
                    threshold: threshold.to_string(),
                })
                .unwrap(),
            })
        }

        // ── §4.5 Interaction request events ───────────────────────────────────

        // HitlPending: ExecutorEvent has no dedicated HitlPending variant.
        // HITL approval is handled via a separate channel (UserInteractionBroker).
        // Skipped here -- noted in issues.

        // AskUserQuestion: ExecutorEvent has no dedicated AskUserQuestion variant.
        // AskUser is handled via UserInteractionBroker directly.
        // Skipped here -- noted in issues.
        ExecutorEvent::RewindCompleted {
            summary, messages, ..
        } => {
            let _ = summary; // summary unused in current preview payload
            let rewind_messages = messages
                .iter()
                .map(|m| RewindMessage {
                    id: m.id().as_uuid().to_string(),
                    role: match m {
                        peri_agent::messages::BaseMessage::Human { .. } => "user".to_string(),
                        peri_agent::messages::BaseMessage::Ai { .. } => "assistant".to_string(),
                        peri_agent::messages::BaseMessage::System { .. } => "system".to_string(),
                        peri_agent::messages::BaseMessage::Tool { .. } => "tool".to_string(),
                    },
                    preview: truncate_text(&m.content(), 200),
                })
                .collect();
            Some(RoutingOutput {
                event_name: "rewind-preview".into(),
                data: serde_json::to_value(&RewindPreview {
                    files: vec![], // RewindCompleted does not carry file changes
                    messages: rewind_messages,
                })
                .unwrap(),
            })
        }

        // OAuthAuthorizationNeeded: ExecutorEvent has no dedicated variant.
        // OAuth is handled via MCP server interaction.
        // Skipped here -- noted in issues.

        // ── §5.1 Discarded events ────────────────────────────────────────────
        ExecutorEvent::LlmCallEnd { .. }
        | ExecutorEvent::LlmRetrying { .. }
        | ExecutorEvent::LspDiagnostics { .. }
        | ExecutorEvent::CompactStarted
        | ExecutorEvent::CompactCompleted { .. }
        | ExecutorEvent::CompactError { .. }
        | ExecutorEvent::LlmCallStart { .. }
        | ExecutorEvent::LlmRequestPayload { .. }
        | ExecutorEvent::MessageAdded(_)
        | ExecutorEvent::StateSnapshot(_)
        | ExecutorEvent::StateSnapshotMeta { .. }
        | ExecutorEvent::BackgroundTaskCompleted(_)
        | ExecutorEvent::BgToolStep { .. }
        | ExecutorEvent::WorkflowProgress(_)
        | ExecutorEvent::TodoUpdate(_)
        // §4.1 streaming events now routed through standard session/update
        | ExecutorEvent::TextChunk { .. }
        | ExecutorEvent::AiReasoning { .. }
        | ExecutorEvent::ToolStart { .. }
        | ExecutorEvent::ToolEnd { .. }
        | ExecutorEvent::TurnCommitted { .. }
        | ExecutorEvent::SubagentStarted { .. }
        | ExecutorEvent::SubagentStopped { .. }
        | ExecutorEvent::AgentExecutionFailed { .. } => None,
        // ── TurnSuspended: idle/await_wake 时发出，通知 TUI 停止 loading ──
        ExecutorEvent::TurnSuspended => Some(RoutingOutput {
            event_name: "turn-suspended".into(),
            data: serde_json::Value::Object(serde_json::Map::new()),
        }),
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────────
// truncate_text 已迁移至 `super::truncate`（与 `view_mapper.rs` 共享），
// 本文件顶部 `use` 引入。RewindPreview 仍使用它来截断消息预览。

// ─── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_call_end_all_discarded() {
        // usage: Some → discarded (token-usage event deprecated, §C)
        let ev_with_usage = ExecutorEvent::LlmCallEnd {
            step: 1,
            model: "test".into(),
            output: "answer".into(),
            usage: Some(peri_agent::llm::types::TokenUsage {
                input_tokens: 500,
                output_tokens: 200,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                request_id: None,
            }),
            stop_reason: None,
        };
        assert!(route(&ev_with_usage).is_none());

        // usage: None → discarded (was already in discarded list)
        let ev_no_usage = ExecutorEvent::LlmCallEnd {
            step: 1,
            model: "test".into(),
            output: "answer".into(),
            usage: None,
            stop_reason: None,
        };
        assert!(route(&ev_no_usage).is_none());
    }

    #[test]
    fn test_context_warning_routes_to_budget_warning() {
        let ev = ExecutorEvent::ContextWarning {
            used_tokens: 85000,
            total_tokens: 100000,
            percentage: 0.85,
        };
        let out = route(&ev).unwrap();
        assert_eq!(out.event_name, "budget-warning");
        assert_eq!(out.data["used"], 85000);
        assert_eq!(out.data["limit"], 100000);
        assert_eq!(out.data["threshold"], "0.85");
    }

    #[test]
    fn test_context_warning_070_threshold() {
        let ev = ExecutorEvent::ContextWarning {
            used_tokens: 70000,
            total_tokens: 100000,
            percentage: 0.70,
        };
        let out = route(&ev).unwrap();
        assert_eq!(out.data["threshold"], "0.70");
    }

    #[test]
    fn test_rewind_completed_routes() {
        let msgs = vec![
            peri_agent::messages::BaseMessage::human(peri_agent::messages::MessageContent::text(
                "hello",
            )),
            peri_agent::messages::BaseMessage::ai(peri_agent::messages::MessageContent::text(
                "world",
            )),
        ];
        let ev = ExecutorEvent::RewindCompleted {
            summary: "rolled back 2 messages".into(),
            messages: msgs,
        };
        let out = route(&ev).unwrap();
        assert_eq!(out.event_name, "rewind-preview");
        assert_eq!(out.data["messages"].as_array().unwrap().len(), 2);
    }

    // ── Discarded events ────────────────────────────────────────────────────

    #[test]
    fn test_llm_retrying_discarded() {
        let ev = ExecutorEvent::LlmRetrying {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 1000,
            error: "rate limited".into(),
        };
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_lsp_diagnostics_discarded() {
        let ev = ExecutorEvent::LspDiagnostics {
            errors: 1,
            warnings: 2,
            files_with_errors: 1,
        };
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_compact_started_discarded() {
        assert!(route(&ExecutorEvent::CompactStarted).is_none());
    }

    #[test]
    fn test_compact_completed_discarded() {
        let ev = ExecutorEvent::CompactCompleted {
            summary: "done".into(),
            files: vec![],
            skills: vec![],
            micro_cleared: 0,
            messages: vec![],
        };
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_compact_error_discarded() {
        let ev = ExecutorEvent::CompactError {
            message: "failed".into(),
        };
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_message_added_discarded() {
        let ev = ExecutorEvent::MessageAdded(peri_agent::messages::BaseMessage::human(
            peri_agent::messages::MessageContent::text("test"),
        ));
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_state_snapshot_discarded() {
        let ev = ExecutorEvent::StateSnapshot(vec![]);
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_llm_call_start_discarded() {
        let ev = ExecutorEvent::LlmCallStart {
            step: 1,
            messages: std::sync::Arc::new(vec![]),
            tools: vec![],
        };
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_llm_request_payload_discarded() {
        let ev = ExecutorEvent::LlmRequestPayload {
            step: 1,
            body: std::sync::Arc::new(serde_json::Value::Null),
        };
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_background_task_completed_discarded() {
        let ev = ExecutorEvent::BackgroundTaskCompleted(
            peri_agent::agent::events::BackgroundTaskResult {
                task_id: "t-1".into(),
                agent_name: "worker".into(),
                prompt_summary: "test".into(),
                success: true,
                output: "done".into(),
                tool_calls_count: 2,
                duration_ms: 1000,
                child_thread_id: None,
            },
        );
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_bg_tool_step_discarded() {
        let ev = ExecutorEvent::BgToolStep {
            child_thread_id: "ct-1".into(),
        };
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_workflow_progress_discarded() {
        let ev =
            ExecutorEvent::WorkflowProgress(peri_agent::agent::events::WorkflowProgressPayload {
                run_id: "r-1".into(),
                workflow_name: "review".into(),
                event_type: "run_started".into(),
                agent_id: None,
                phase: None,
                label: None,
                agent_status: None,
                token_count: None,
                tool_count: None,
                run_status: None,
                message: None,
            });
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_todo_update_discarded() {
        let ev = ExecutorEvent::TodoUpdate(vec![]);
        assert!(route(&ev).is_none());
    }

    #[test]
    fn test_state_snapshot_meta_discarded() {
        let ev = ExecutorEvent::StateSnapshotMeta {
            message_count: 10,
            total_tokens: 5000,
            current_step: 3,
            consecutive_failures: 0,
            budget_pct: Some(0.5),
            context_total_tokens: Some(200_000),
        };
        assert!(route(&ev).is_none());
    }

    // ── Helper tests ─────────────────────────────────────────────────────────
    // summarize_input / summarize_output / truncate_text 的单元测试
    // 已随实现一起迁移至 `super::truncate` 模块，这里不再重复。
}
