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
#[path = "router_test.rs"]
mod tests;
