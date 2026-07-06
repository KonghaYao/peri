//! Event router -- maps `ExecutorEvent` to `peri/unstable-event` protocol payloads.
//!
//! Each call to [`route`] returns an optional [`RoutingOutput`] containing:
//! - `event_name`: kebab-case string identifying the event (e.g. `"text-chunk"`)
//! - `data`: JSON value carrying the event-specific payload
//!
//! Events that are internal to the agent (LLM retries, compact lifecycle, etc.)
//! return `None` and are discarded per §5.1 of the protocol design doc.
//!
//! Reference: `docs/design/peri-acp-protocol.md` section 5 "Event Router".

use peri_acp_types::event_data::*;
use peri_agent::agent::events::ExecutorEvent;

/// Output of [`route`] -- an event name + its JSON data payload.
#[derive(Debug, Clone)]
pub struct RoutingOutput {
    /// kebab-case event name (e.g. `"text-chunk"`, `"view-commit"`).
    pub event_name: String,
    /// Serialized event data.
    pub data: serde_json::Value,
}

/// Trait for converting finalized messages into [`peri_acp_types::view_model::ViewModel`]s.
///
/// The ACP layer injects a concrete implementation at construction time.
/// The router itself is agnostic to the conversion logic.
pub trait ViewMapper {
    /// Convert a list of finalized `BaseMessage`s into `ViewModel`s.
    fn convert(
        &mut self,
        messages: &[peri_agent::messages::BaseMessage],
    ) -> Vec<peri_acp_types::view_model::ViewModel>;
}

/// Map an [`ExecutorEvent`] to a `peri/unstable-event` routing output.
///
/// Returns `None` for events that should be discarded (§5.1):
/// - `LlmRetrying` -- internal retry behavior
/// - `LspDiagnostics` -- only relevant within tool execution context
/// - `CompactStarted` / `CompactCompleted` / `CompactError` -- transparent to user
/// - `LlmCallStart` / `LlmRequestPayload` -- observability-only
/// - `MessageAdded` -- incremental, not needed for TUI rendering
/// - `StateSnapshot` -- superseded by `TurnCommitted` for rendering
/// - `StateSnapshotMeta` -- status-bar metadata, not a push event
/// - `LlmCallEnd` with usage -- status-bar metadata (token counts), not a push event
/// - `LlmCallEnd` without usage -- filtered
/// - `BackgroundTaskCompleted` / `BgToolStep` -- handled via separate TUI polling
/// - `WorkflowProgress` -- handled via dedicated panel
/// - `AgentExecutionFailed` -- routed as `"turn-interrupted"` so the v2 state machine can exit Streaming
pub fn route(ev: &ExecutorEvent, view_mapper: &mut dyn ViewMapper) -> Option<RoutingOutput> {
    match ev {
        // ── §4.1 Streaming events ───────────────────────────────────────────
        ExecutorEvent::TextChunk {
            chunk,
            source_agent_id,
            ..
        } => Some(RoutingOutput {
            event_name: "text-chunk".into(),
            data: serde_json::to_value(&TextChunk {
                text: chunk.clone(),
                agent_id: source_agent_id.clone(),
            })
            .unwrap(),
        }),

        ExecutorEvent::AiReasoning {
            text,
            source_agent_id,
        } => Some(RoutingOutput {
            event_name: "reasoning-chunk".into(),
            data: serde_json::to_value(&ReasoningChunk {
                text: text.clone(),
                agent_id: source_agent_id.clone(),
            })
            .unwrap(),
        }),

        ExecutorEvent::ToolStart {
            tool_call_id,
            name,
            input,
            source_agent_id,
            ..
        } => Some(RoutingOutput {
            event_name: "tool-started".into(),
            data: serde_json::to_value(&ToolStarted {
                tool_id: tool_call_id.clone(),
                tool_name: name.clone(),
                input_summary: summarize_input(name, input),
                agent_id: source_agent_id.clone(),
            })
            .unwrap(),
        }),

        ExecutorEvent::ToolEnd {
            tool_call_id,
            name,
            output,
            is_error,
            source_agent_id,
            ..
        } => Some(RoutingOutput {
            event_name: "tool-ended".into(),
            data: serde_json::to_value(&ToolEnded {
                tool_id: tool_call_id.clone(),
                output_summary: summarize_output(name, output),
                is_error: *is_error,
                agent_id: source_agent_id.clone(),
            })
            .unwrap(),
        }),

        // ── §4.2 Boundary events ─────────────────────────────────────────────
        ExecutorEvent::TurnCommitted { messages, .. } => {
            let view_models = view_mapper.convert(messages);
            Some(RoutingOutput {
                event_name: "view-commit".into(),
                data: serde_json::to_value(&ViewCommit { view_models }).unwrap(),
            })
        }

        // ── §4.3 Status events ───────────────────────────────────────────────
        ExecutorEvent::LlmCallEnd { usage: Some(u), .. } => Some(RoutingOutput {
            event_name: "token-usage".into(),
            data: serde_json::to_value(&TokenUsage {
                input: u.input_tokens as u64,
                output: u.output_tokens as u64,
            })
            .unwrap(),
        }),

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

        // ── §4.6 Structure events ─────────────────────────────────────────────
        ExecutorEvent::SubagentStarted {
            agent_name,
            instance_id,
            ..
        } => Some(RoutingOutput {
            event_name: "subagent-started".into(),
            data: serde_json::to_value(&SubagentStarted {
                agent_id: instance_id.clone(),
                agent_name: agent_name.clone(),
            })
            .unwrap(),
        }),

        ExecutorEvent::SubagentStopped { instance_id, .. } => Some(RoutingOutput {
            event_name: "subagent-stopped".into(),
            data: serde_json::to_value(&SubagentStopped {
                agent_id: instance_id.clone(),
            })
            .unwrap(),
        }),

        // ── §5.1 Discarded events ────────────────────────────────────────────
        ExecutorEvent::LlmRetrying { .. }
        | ExecutorEvent::LspDiagnostics { .. }
        | ExecutorEvent::CompactStarted
        | ExecutorEvent::CompactCompleted { .. }
        | ExecutorEvent::CompactError { .. }
        | ExecutorEvent::LlmCallStart { .. }
        | ExecutorEvent::LlmRequestPayload { .. }
        | ExecutorEvent::MessageAdded(_)
        | ExecutorEvent::StateSnapshot(_)
        | ExecutorEvent::StateSnapshotMeta { .. }
        | ExecutorEvent::LlmCallEnd { usage: None, .. }
        | ExecutorEvent::BackgroundTaskCompleted(_)
        | ExecutorEvent::BgToolStep { .. }
        | ExecutorEvent::WorkflowProgress(_)
        | ExecutorEvent::TodoUpdate(_) => None,

        // ── §4.6 Terminal events ────────────────────────────────────────────
        ExecutorEvent::AgentExecutionFailed { message } => Some(RoutingOutput {
            event_name: "turn-interrupted".into(),
            data: serde_json::to_value(TurnInterrupted {
                reason: message.clone(),
            })
            .unwrap_or(serde_json::json!({ "reason": message })),
        }),
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────────

/// Produce a one-line summary of a tool's JSON input.
///
/// Tool-specific formatting per TUI-TOOLCALL.md §2:
/// - Read/Write/Edit: `file_path` value only
/// - Bash: `command` value only
/// - Glob/Grep: `pattern: "{pattern}"`, pattern truncated to 200 chars
/// - folder_operations: `"{operation} {folder_path}"`
/// - WebSearch: `query: "{query}"`, query truncated to 60 chars
/// - WebFetch: `url: {url}`, no truncation
/// - TodoWrite: empty string
/// - AgentResult: `task_id`, truncated to 12 chars
/// - artifact: `file_path` value only
/// - LSP: `operation`, truncated to 40 chars
/// - ExecuteExtraTool: `tool_name`, truncated to 40 chars
/// - SearchExtraTools: `query`, truncated to 40 chars
/// - Other: fallback to first key-value pair
fn summarize_input(name: &str, input: &serde_json::Value) -> String {
    // Helper to extract a string field
    let field = |key: &str| -> Option<&str> { input.get(key).and_then(|v| v.as_str()) };

    match name {
        "Read" | "Write" | "Edit" => field("file_path").unwrap_or("(empty input)").to_string(),
        "Bash" => field("command").unwrap_or("(empty input)").to_string(),
        "Glob" | "Grep" => field("pattern")
            .map(|s| format!(r#"pattern: "{}""#, truncate_text(s, 200)))
            .unwrap_or_else(|| "(empty input)".to_string()),
        "folder_operations" => {
            let op = field("operation").unwrap_or("");
            let path = field("folder_path").unwrap_or("");
            format!("{} {}", op, path)
        }
        "WebSearch" => field("query")
            .map(|s| format!(r#"query: "{}""#, truncate_text(s, 60)))
            .unwrap_or_else(|| "(empty input)".to_string()),
        "WebFetch" => field("url")
            .map(|s| format!("url: {}", s))
            .unwrap_or_else(|| "(empty input)".to_string()),
        "TodoWrite" => String::new(),
        "AgentResult" => field("task_id")
            .map(|s| truncate_text(s, 12))
            .unwrap_or_default(),
        "artifact" => field("file_path").unwrap_or("").to_string(),
        "LSP" => field("operation")
            .map(|s| truncate_text(s, 40))
            .unwrap_or_default(),
        "ExecuteExtraTool" => field("tool_name")
            .map(|s| truncate_text(s, 40))
            .unwrap_or_default(),
        "SearchExtraTools" => field("query")
            .map(|s| truncate_text(s, 40))
            .unwrap_or_default(),
        _ => {
            // Fallback: existing generic logic for unknown tools
            match input {
                serde_json::Value::Object(map) => {
                    if let Some(path) = map.get("path").or_else(|| map.get("file_path")) {
                        return format!("path: {}", truncate_text(&path.to_string(), 120));
                    }
                    if let Some(query) = map.get("query").or_else(|| map.get("pattern")) {
                        return format!("query: {}", truncate_text(&query.to_string(), 120));
                    }
                    if let Some(cmd) = map.get("command") {
                        return format!("cmd: {}", truncate_text(&cmd.to_string(), 120));
                    }
                    if let Some((k, v)) = map.iter().next() {
                        return format!("{}: {}", k, truncate_text(&v.to_string(), 100));
                    }
                    "(empty input)".to_string()
                }
                other => truncate_text(&other.to_string(), 120),
            }
        }
    }
}

/// Produce a one-line summary of a tool's output.
fn summarize_output(name: &str, output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // For diff-producing tools, count changed lines
    if matches!(name, "Edit" | "Write") {
        let lines = trimmed.lines().count();
        if lines <= 3 {
            return truncate_text(trimmed, 200);
        }
        format!("{} lines changed", lines)
    } else {
        truncate_text(trimmed, 200)
    }
}

/// Truncate text to `max_chars` Unicode code points.
fn truncate_text(s: &str, max_chars: usize) -> String {
    let len = s.chars().count();
    if len <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// No-op ViewMapper for testing.
    struct NopViewMapper;

    impl ViewMapper for NopViewMapper {
        fn convert(
            &mut self,
            _messages: &[peri_agent::messages::BaseMessage],
        ) -> Vec<peri_acp_types::view_model::ViewModel> {
            vec![]
        }
    }

    #[test]
    fn test_text_chunk_routes() {
        let ev = ExecutorEvent::TextChunk {
            message_id: Default::default(),
            chunk: "hello world".into(),
            source_agent_id: None,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "text-chunk");
        assert_eq!(out.data["text"], "hello world");
        assert!(out.data.get("agent_id").unwrap().is_null());
    }

    #[test]
    fn test_text_chunk_with_subagent_routes() {
        let ev = ExecutorEvent::TextChunk {
            message_id: Default::default(),
            chunk: "sub output".into(),
            source_agent_id: Some("sa-1".into()),
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.data["agent_id"], "sa-1");
    }

    #[test]
    fn test_reasoning_chunk_routes() {
        let ev = ExecutorEvent::AiReasoning {
            text: "thinking...".into(),
            source_agent_id: None,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "reasoning-chunk");
        assert_eq!(out.data["text"], "thinking...");
        assert!(out.data["agent_id"].is_null());
    }

    #[test]
    fn test_reasoning_chunk_with_subagent_routes() {
        let ev = ExecutorEvent::AiReasoning {
            text: "sub thinking".into(),
            source_agent_id: Some("sa-1".into()),
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "reasoning-chunk");
        assert_eq!(out.data["text"], "sub thinking");
        assert_eq!(out.data["agent_id"], "sa-1");
    }

    #[test]
    fn test_tool_start_routes() {
        let ev = ExecutorEvent::ToolStart {
            message_id: Default::default(),
            tool_call_id: "tc-1".into(),
            name: "Read".into(),
            input: serde_json::json!({"file_path": "/tmp/foo.rs"}),
            source_agent_id: None,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "tool-started");
        assert_eq!(out.data["tool_id"], "tc-1");
        assert_eq!(out.data["tool_name"], "Read");
        assert_eq!(out.data["input_summary"], "/tmp/foo.rs");
    }

    #[test]
    fn test_tool_end_routes() {
        let ev = ExecutorEvent::ToolEnd {
            message_id: Default::default(),
            tool_call_id: "tc-1".into(),
            name: "Bash".into(),
            output: "done\n".into(),
            is_error: false,
            source_agent_id: None,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "tool-ended");
        assert_eq!(out.data["tool_id"], "tc-1");
        assert_eq!(out.data["is_error"], false);
    }

    #[test]
    fn test_tool_end_error_routes() {
        let ev = ExecutorEvent::ToolEnd {
            message_id: Default::default(),
            tool_call_id: "tc-2".into(),
            name: "Bash".into(),
            output: "command not found".into(),
            is_error: true,
            source_agent_id: None,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.data["is_error"], true);
    }

    #[test]
    fn test_turn_committed_routes_to_view_commit() {
        let ev = ExecutorEvent::TurnCommitted {
            messages: vec![],
            steps: 3,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "view-commit");
        // NopViewMapper returns empty list
        assert_eq!(out.data["view_models"], serde_json::json!([]));
    }

    #[test]
    fn test_token_usage_routes() {
        let ev = ExecutorEvent::LlmCallEnd {
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
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "token-usage");
        assert_eq!(out.data["input"], 500);
        assert_eq!(out.data["output"], 200);
    }

    #[test]
    fn test_llm_call_end_no_usage_discarded() {
        let ev = ExecutorEvent::LlmCallEnd {
            step: 1,
            model: "test".into(),
            output: "answer".into(),
            usage: None,
            stop_reason: None,
        };
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_context_warning_routes_to_budget_warning() {
        let ev = ExecutorEvent::ContextWarning {
            used_tokens: 85000,
            total_tokens: 100000,
            percentage: 0.85,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
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
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.data["threshold"], "0.70");
    }

    #[test]
    fn test_subagent_started_routes() {
        let ev = ExecutorEvent::SubagentStarted {
            agent_name: "researcher".into(),
            instance_id: "sa-42".into(),
            is_background: false,
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "subagent-started");
        assert_eq!(out.data["agent_id"], "sa-42");
        assert_eq!(out.data["agent_name"], "researcher");
    }

    #[test]
    fn test_subagent_stopped_routes() {
        let ev = ExecutorEvent::SubagentStopped {
            agent_name: "researcher".into(),
            result: "done".into(),
            is_error: false,
            instance_id: "sa-42".into(),
        };
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
        assert_eq!(out.event_name, "subagent-stopped");
        assert_eq!(out.data["agent_id"], "sa-42");
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
        let mut mapper = NopViewMapper;
        let out = route(&ev, &mut mapper).unwrap();
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
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_lsp_diagnostics_discarded() {
        let ev = ExecutorEvent::LspDiagnostics {
            errors: 1,
            warnings: 2,
            files_with_errors: 1,
        };
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_compact_started_discarded() {
        let mut mapper = NopViewMapper;
        assert!(route(&ExecutorEvent::CompactStarted, &mut mapper).is_none());
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
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_compact_error_discarded() {
        let ev = ExecutorEvent::CompactError {
            message: "failed".into(),
        };
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_message_added_discarded() {
        let ev = ExecutorEvent::MessageAdded(peri_agent::messages::BaseMessage::human(
            peri_agent::messages::MessageContent::text("test"),
        ));
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_state_snapshot_discarded() {
        let ev = ExecutorEvent::StateSnapshot(vec![]);
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_llm_call_start_discarded() {
        let ev = ExecutorEvent::LlmCallStart {
            step: 1,
            messages: std::sync::Arc::new(vec![]),
            tools: vec![],
        };
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_llm_request_payload_discarded() {
        let ev = ExecutorEvent::LlmRequestPayload {
            step: 1,
            body: std::sync::Arc::new(serde_json::Value::Null),
        };
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
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
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_bg_tool_step_discarded() {
        let ev = ExecutorEvent::BgToolStep {
            child_thread_id: "ct-1".into(),
        };
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
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
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    #[test]
    fn test_agent_execution_failed_routes_to_turn_interrupted() {
        let ev = ExecutorEvent::AgentExecutionFailed {
            message: "oom".into(),
        };
        let mut mapper = NopViewMapper;
        let output = route(&ev, &mut mapper).expect("AgentExecutionFailed should route");
        assert_eq!(output.event_name, "turn-interrupted");
        let reason = output.data["reason"].as_str().unwrap();
        assert_eq!(reason, "oom");
    }

    #[test]
    fn test_todo_update_discarded() {
        let ev = ExecutorEvent::TodoUpdate(vec![]);
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
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
        let mut mapper = NopViewMapper;
        assert!(route(&ev, &mut mapper).is_none());
    }

    // ── Helper tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_summarize_input_path() {
        let input = serde_json::json!({"file_path": "/tmp/foo.rs", "offset": 10});
        assert_eq!(summarize_input("Read", &input), "/tmp/foo.rs");
    }

    #[test]
    fn test_summarize_input_query() {
        let input = serde_json::json!({"query": "TODO", "glob": "*.rs"});
        assert_eq!(summarize_input("WebSearch", &input), r#"query: "TODO""#);
    }

    #[test]
    fn test_summarize_input_command() {
        let input = serde_json::json!({"command": "cargo build"});
        assert_eq!(summarize_input("Bash", &input), "cargo build");
    }

    #[test]
    fn test_summarize_input_empty_object() {
        let input = serde_json::json!({});
        assert_eq!(summarize_input("Read", &input), "(empty input)");
    }

    #[test]
    fn test_truncate_text_short() {
        assert_eq!(truncate_text("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_text_exact() {
        assert_eq!(truncate_text("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_text_long() {
        let long = "abcdefghij";
        assert_eq!(truncate_text(long, 5), "abcde...");
    }

    #[test]
    fn test_truncate_text_cjk() {
        // CJK: each char is 1 code point (chars().count), not 1 byte
        let cjk = "你好世界";
        assert_eq!(truncate_text(cjk, 2), "你好...");
    }
}
