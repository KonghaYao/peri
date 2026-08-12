//! Event mapping from ExecutorEvent to ACP SessionUpdate.
//!
//! Produces [`MappedEvent`] structs:
//! - **Category ①** (标准 ACP): TextChunk, AiReasoning, ToolStart, ToolEnd, TodoUpdate,
//!   LlmCallEnd(usage), MessageAdded → `updates` with SessionUpdate
//! - **Other variants**: no SessionUpdate output

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, MessageId, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
    SessionUpdate, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
    ToolKind, UsageUpdate,
};
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::PeriCaps;

/// Result of mapping a single [`ExecutorEvent`].
///
/// Each ExecutorEvent produces zero or more `MappedEvent`s carrying:
/// - `updates`: standard ACP [`SessionUpdate`] list (for IDE/stdio clients)
/// - `source_agent_id`: SubAgent routing hint
#[derive(Debug)]
pub struct MappedEvent {
    pub updates: Vec<SessionUpdate>,
    pub source_agent_id: Option<String>,
}

impl MappedEvent {
    /// Category ①: full SessionUpdate.
    pub fn standard(updates: Vec<SessionUpdate>) -> Self {
        Self {
            updates,
            source_agent_id: None,
        }
    }

    /// Category ① with source_agent_id extracted from the event.
    pub fn standard_with_src(updates: Vec<SessionUpdate>, source_agent_id: Option<String>) -> Self {
        Self {
            updates,
            source_agent_id,
        }
    }
}

/// 将 ExecutorEvent 映射为 [`MappedEvent`] 列表。
///
/// `context_window` 是当前模型的上下文窗口大小（tokens），用于填充 UsageUpdate.size。
///
/// - ① 标准 ACP（IDE）：SessionUpdate 序列化（7 个 SessionUpdate 变体）
/// - 其余所有变体：无 SessionUpdate 输出
pub fn map_event(event: &ExecutorEvent, context_window: u32, caps: &PeriCaps) -> Vec<MappedEvent> {
    match event {
        // ── Category ①: Full SessionUpdate ─────────────────────────────────────────
        ExecutorEvent::TextChunk {
            chunk,
            message_id,
            source_agent_id,
            ..
        } => {
            vec![MappedEvent::standard_with_src(
                vec![SessionUpdate::AgentMessageChunk(
                    ContentChunk::new(ContentBlock::Text(TextContent::new(chunk.clone())))
                        // ACP 标准 messageId 语义：同一消息的 chunk 共享 ID，
                        // 变化即新消息（客户端据此做段边界与推理结束推断）。
                        // v1 wire 上的 messageId 是字符串（规范消息 ID 的 UUID 串）。
                        .message_id(MessageId::from(message_id.as_uuid().to_string())),
                )],
                source_agent_id.clone(),
            )]
        }

        ExecutorEvent::AiReasoning {
            text,
            message_id,
            source_agent_id,
            ..
        } => {
            vec![MappedEvent::standard_with_src(
                vec![SessionUpdate::AgentThoughtChunk(
                    ContentChunk::new(ContentBlock::Text(TextContent::new(text.clone())))
                        .message_id(MessageId::from(message_id.as_uuid().to_string())),
                )],
                source_agent_id.clone(),
            )]
        }

        ExecutorEvent::ToolStart {
            tool_call_id,
            name,
            input,
            source_agent_id,
            ..
        } => {
            vec![MappedEvent::standard_with_src(
                vec![SessionUpdate::ToolCall(
                    ToolCall::new(tool_call_id.clone(), name.clone())
                        .kind(infer_tool_kind(name))
                        .status(ToolCallStatus::InProgress)
                        .raw_input(Some(input.clone())),
                )],
                source_agent_id.clone(),
            )]
        }

        ExecutorEvent::ToolEnd {
            tool_call_id,
            name,
            output,
            is_error,
            source_agent_id,
            ..
        } => {
            let raw_output = match serde_json::from_str::<serde_json::Value>(output) {
                Ok(v) => Some(v),
                Err(_) => Some(serde_json::Value::String(output.clone())),
            };
            vec![MappedEvent::standard_with_src(
                vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    tool_call_id.clone(),
                    ToolCallUpdateFields::new()
                        .title(name.clone())
                        .status(if *is_error {
                            ToolCallStatus::Failed
                        } else {
                            ToolCallStatus::Completed
                        })
                        .raw_output(raw_output),
                ))],
                source_agent_id.clone(),
            )]
        }

        ExecutorEvent::TodoUpdate(entries) => {
            let plan_entries: Vec<PlanEntry> = entries
                .iter()
                .map(|e| {
                    PlanEntry::new(
                        e.content.clone(),
                        PlanEntryPriority::Medium,
                        match e.status {
                            peri_acp_types::event::TodoStatus::Pending => PlanEntryStatus::Pending,
                            peri_acp_types::event::TodoStatus::InProgress => {
                                PlanEntryStatus::InProgress
                            }
                            peri_acp_types::event::TodoStatus::Completed => {
                                PlanEntryStatus::Completed
                            }
                        },
                    )
                })
                .collect();
            vec![MappedEvent::standard(vec![SessionUpdate::Plan(Plan::new(
                plan_entries,
            ))])]
        }

        ExecutorEvent::LlmCallEnd {
            usage: Some(u),
            model,
            stop_reason,
            request_id,
            ..
        } => {
            let update = UsageUpdate::new(
                u64::from(u.input_tokens) + u64::from(u.output_tokens),
                u64::from(context_window),
            );
            // 只有当 tokenStats cap 为 true 时才附加 _meta
            let update = if caps.token_stats {
                let mut meta = serde_json::Map::new();
                meta.insert("inputTokens".into(), serde_json::json!(u.input_tokens));
                meta.insert("outputTokens".into(), serde_json::json!(u.output_tokens));
                if let Some(v) = u.cache_creation_input_tokens {
                    meta.insert("cacheCreationTokens".into(), serde_json::json!(v));
                }
                if let Some(v) = u.cache_read_input_tokens {
                    meta.insert("cacheReadTokens".into(), serde_json::json!(v));
                }
                if let Some(ref rid) = request_id {
                    meta.insert("requestId".into(), serde_json::json!(rid));
                }
                meta.insert("model".into(), serde_json::json!(model));
                if let Some(ref sr) = stop_reason {
                    meta.insert("stopReason".into(), serde_json::json!(stop_reason_wire(sr)));
                }
                update.meta(meta)
            } else {
                update
            };

            vec![MappedEvent::standard(vec![SessionUpdate::UsageUpdate(
                update,
            )])]
        }

        // ── Synthetic user message (Category ①) ─────────────────────────────────
        ExecutorEvent::MessageAdded(msg) => {
            let text = msg.content();
            vec![MappedEvent {
                updates: vec![SessionUpdate::UserMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text.to_string())),
                ))],
                source_agent_id: None,
            }]
        }

        // LlmCallEnd usage=None（LLM 调用失败/异常）：无 UsageUpdate 输出。
        ExecutorEvent::LlmCallEnd { usage: None, .. } => vec![MappedEvent::standard(vec![])],

        // ── All other variants: no SessionUpdate output ──────────────────────────
        // 显式穷尽（`2026-07-25-event-identity-diverges-across-dual-delivery-paths.md`）：
        // 每个 ExecutorEvent 变体必须显式列出，新增变体无法静默落入 wildcard 丢弃分支。
        // 这些变体或经 peri/agent_event DTO 通道送达 TUI（SubagentStarted/Stopped、
        // CompactCompleted、AgentExecutionFailed、RewindCompleted/Error、
        // TurnSuspended 等，见 event_sink.rs），或为 Langfuse/tracer-only（Stage*、
        // TurnStarted/Ended、LlmCallStart/RequestPayload、BudgetThresholdHit 等）。
        ExecutorEvent::StateSnapshot(_)
        | ExecutorEvent::TurnCommitted { .. }
        | ExecutorEvent::StateSnapshotMeta { .. }
        | ExecutorEvent::TurnSuspended { .. }
        | ExecutorEvent::LlmCallStart { .. }
        | ExecutorEvent::LlmRequestPayload { .. }
        | ExecutorEvent::ContextWarning { .. }
        | ExecutorEvent::LlmRetrying { .. }
        | ExecutorEvent::BackgroundTaskCompleted(_)
        | ExecutorEvent::SubagentStarted { .. }
        | ExecutorEvent::SubagentStopped { .. }
        | ExecutorEvent::CompactStarted { .. }
        | ExecutorEvent::CompactCompleted { .. }
        | ExecutorEvent::RewindCompleted { .. }
        | ExecutorEvent::RewindError { .. }
        | ExecutorEvent::CompactError { .. }
        | ExecutorEvent::AgentExecutionFailed { .. }
        | ExecutorEvent::LspDiagnostics { .. }
        | ExecutorEvent::BgToolStep { .. }
        | ExecutorEvent::WorkflowProgress(_)
        | ExecutorEvent::SessionStarted { .. }
        | ExecutorEvent::TurnStarted { .. }
        | ExecutorEvent::TurnEnded { .. }
        | ExecutorEvent::MiddlewareStarted { .. }
        | ExecutorEvent::MiddlewareEnded { .. }
        | ExecutorEvent::BudgetThresholdHit { .. }
        | ExecutorEvent::WorkflowStarted { .. }
        | ExecutorEvent::WorkflowEnded { .. }
        | ExecutorEvent::SystemNotification { .. }
        | ExecutorEvent::OauthNeeded { .. }
        | ExecutorEvent::OauthCompleted { .. }
        | ExecutorEvent::OauthFailed { .. }
        | ExecutorEvent::BgRegistryEvent(_) => {
            vec![MappedEvent::standard(vec![])]
        }
    }
}

fn infer_tool_kind(name: &str) -> ToolKind {
    match name {
        "Read" => ToolKind::Read,
        "Write" | "Edit" | "folder_operations" => ToolKind::Edit,
        "Bash" => ToolKind::Execute,
        "Grep" | "Glob" => ToolKind::Search,
        "WebFetch" | "WebSearch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

/// `stop_reason` 的 legacy wire format 字符串（与历史 StopReason Display 及
/// JSON 字段值一致，如 "end_turn"）。`peri_model::StopReason` 无 `Display`，
/// 此处显式映射；不能退化为 `{:?}` 的变体名，否则 ACP `_meta.stopReason`
/// 会输出 "EndTurn"。
fn stop_reason_wire(reason: &peri_model::StopReason) -> String {
    match reason {
        peri_model::StopReason::EndTurn => "end_turn".into(),
        peri_model::StopReason::ToolUse => "tool_use".into(),
        peri_model::StopReason::MaxTokens => "max_tokens".into(),
        peri_model::StopReason::Other { value } => value.clone(),
    }
}

#[cfg(test)]
#[path = "mapper_test.rs"]
mod tests;
