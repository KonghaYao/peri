//! Event mapping from ExecutorEvent to ACP SessionUpdate and peri/agent_event routing.
//!
//! Produces [`MappedEvent`] structs with four routing categories:
//! - **Category ①** (标准 ACP): TextChunk, AiReasoning, ToolStart, ToolEnd, TodoUpdate,
//!   LlmCallEnd(usage) → `updates` only, `forward_to_tui: false`
//! - **Category ②** (HITL 审批): HitlPending 事件 → `hitl_pending: true`
//! - **Category ③** (TUI-only): StateSnapshot, Subagent*, Compact*, ContextWarning, LlmRetrying, etc.
//!   → `forward_to_tui: true` only
//! - **Category ④** (观测层): AgentLifecycle, TurnCompleted → `observable: true`
//! - **Filtered**: LlmCallStart, LlmCallEnd(usage:None), LlmRequestPayload
//!   → empty
//! - **Synthetic (Category ①)**: MessageAdded → `user_message_chunk` session/update
//!   → injects synthetic human messages (bg agent callback etc.) into TUI committed.
//!   Note: 同时走 unstable event 通道（executor_helpers）发送 BgCallbackBubble 做 flush，
//!   session/update 通道负责推送气泡内容，unstable event 通道负责切分 visual turn。

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionUpdate,
    TextContent, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
    UsageUpdate,
};
use peri_agent::agent::events::ExecutorEvent;

/// Result of mapping a single [`ExecutorEvent`].
///
/// Each ExecutorEvent produces zero or more `MappedEvent`s carrying:
/// - `updates`: standard ACP [`SessionUpdate`] list (for IDE/stdio clients)
/// - `forward_to_tui`: whether the event should also be sent via `peri/agent_event`
/// - `hitl_pending`: whether the event is a HITL approval request (broadcast channel)
/// - `observable`: whether the event should be broadcast to observability subscribers
/// - `source_agent_id`: SubAgent routing hint
#[derive(Debug)]
pub struct MappedEvent {
    pub updates: Vec<SessionUpdate>,
    pub forward_to_tui: bool,
    pub hitl_pending: bool,
    pub observable: bool,
    pub source_agent_id: Option<String>,
}

impl MappedEvent {
    /// Category ①: full SessionUpdate, no TUI forwarding.
    pub fn standard(updates: Vec<SessionUpdate>) -> Self {
        Self {
            updates,
            forward_to_tui: false,
            hitl_pending: false,
            observable: false,
            source_agent_id: None,
        }
    }

    /// Category ① with source_agent_id extracted from the event.
    pub fn standard_with_src(updates: Vec<SessionUpdate>, source_agent_id: Option<String>) -> Self {
        Self {
            updates,
            forward_to_tui: false,
            hitl_pending: false,
            observable: false,
            source_agent_id,
        }
    }

    /// Category ③: TUI-only, no SessionUpdate.
    pub fn tui_only() -> Self {
        Self {
            updates: vec![],
            forward_to_tui: true,
            hitl_pending: false,
            observable: false,
            source_agent_id: None,
        }
    }

    /// Category ②: both SessionUpdate and TUI forwarding.
    pub fn both(updates: Vec<SessionUpdate>) -> Self {
        Self {
            updates,
            forward_to_tui: true,
            hitl_pending: false,
            observable: false,
            source_agent_id: None,
        }
    }

    /// Category ④: 观测层事件（broadcast 给外部监听器）
    pub fn observable() -> Self {
        Self {
            updates: vec![],
            forward_to_tui: false,
            hitl_pending: false,
            observable: true,
            source_agent_id: None,
        }
    }

    /// Category ②: HITL 审批事件
    pub fn hitl() -> Self {
        Self {
            updates: vec![],
            forward_to_tui: true,
            hitl_pending: true,
            observable: false,
            source_agent_id: None,
        }
    }

    /// Category ③ + ④: TUI-only 且可观测
    pub fn tui_and_observable() -> Self {
        Self {
            updates: vec![],
            forward_to_tui: true,
            observable: true,
            hitl_pending: false,
            source_agent_id: None,
        }
    }

    /// Filtered: no output at all.
    pub fn none() -> Self {
        Self {
            updates: vec![],
            forward_to_tui: false,
            hitl_pending: false,
            observable: false,
            source_agent_id: None,
        }
    }
}

/// 将 ExecutorEvent 映射为 [`MappedEvent`] 列表。
///
/// `context_window` 是当前模型的上下文窗口大小（tokens），用于填充 UsageUpdate.size。
///
/// 四路分路：
/// - ① 标准 ACP（IDE）：SessionUpdate 序列化
/// - ② HITL 审批：broadcast 独立审批通道
/// - ③ TUI 专用：peri/agent_event 通知
/// - ④ 观测层：broadcast 给外部监听器
pub fn map_event(event: &ExecutorEvent, context_window: u32) -> Vec<MappedEvent> {
    match event {
        // ── Category ①: Full SessionUpdate ─────────────────────────────────────────
        ExecutorEvent::TextChunk {
            chunk,
            source_agent_id,
            ..
        } => {
            vec![MappedEvent::standard_with_src(
                vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(chunk.clone())),
                ))],
                source_agent_id.clone(),
            )]
        }

        ExecutorEvent::AiReasoning {
            text,
            source_agent_id,
            ..
        } => {
            vec![MappedEvent::standard_with_src(
                vec![SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text.clone())),
                ))],
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
                            peri_agent::agent::events::TodoStatus::Pending => {
                                PlanEntryStatus::Pending
                            }
                            peri_agent::agent::events::TodoStatus::InProgress => {
                                PlanEntryStatus::InProgress
                            }
                            peri_agent::agent::events::TodoStatus::Completed => {
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
            ..
        } => {
            let mut meta = serde_json::Map::new();
            meta.insert("inputTokens".into(), serde_json::json!(u.input_tokens));
            meta.insert("outputTokens".into(), serde_json::json!(u.output_tokens));
            if let Some(v) = u.cache_creation_input_tokens {
                meta.insert("cacheCreationTokens".into(), serde_json::json!(v));
            }
            if let Some(v) = u.cache_read_input_tokens {
                meta.insert("cacheReadTokens".into(), serde_json::json!(v));
            }
            if let Some(ref rid) = u.request_id {
                meta.insert("requestId".into(), serde_json::json!(rid));
            }
            meta.insert("model".into(), serde_json::json!(model));
            if let Some(ref sr) = stop_reason {
                meta.insert("stopReason".into(), serde_json::json!(sr.to_string()));
            }

            vec![MappedEvent::standard(vec![SessionUpdate::UsageUpdate(
                UsageUpdate::new(
                    u64::from(u.input_tokens) + u64::from(u.output_tokens),
                    u64::from(context_window),
                )
                .meta(meta),
            )])]
        }

        // ── Category ②: HITL 审批（broadcast 独立审批通道）────────────────────────
        // 注：当前 ExecutorEvent 中无专门的 HitlPending 变体，
        // HITL 审批通过 UserInteractionBroker 的 ask/confirm 直接交互，
        // 不经过事件管道。此处预留 Category ② 路由位，
        // 未来 ExecutorEvent 扩展 HitlPending 时可直接启用。

        // ── Category ③: TUI-only (no SessionUpdate) ──────────────────────────────
        ExecutorEvent::ContextWarning { .. }
        | ExecutorEvent::LlmRetrying { .. }
        | ExecutorEvent::StateSnapshot(_)
        | ExecutorEvent::StateSnapshotMeta { .. }
        | ExecutorEvent::TurnCommitted { .. }
        | ExecutorEvent::CompactStarted
        | ExecutorEvent::CompactCompleted { .. }
        | ExecutorEvent::CompactError { .. }
        | ExecutorEvent::RewindCompleted { .. }
        | ExecutorEvent::BackgroundTaskCompleted(_)
        | ExecutorEvent::BgToolStep { .. }
        | ExecutorEvent::LspDiagnostics { .. }
        | ExecutorEvent::AgentExecutionFailed { .. }
        | ExecutorEvent::WorkflowProgress(_) => {
            vec![MappedEvent::tui_only()]
        }

        // ── Category ③ + ④: TUI-only 且可观测（SubAgent 生命周期）──────────────
        ExecutorEvent::SubagentStarted { .. } | ExecutorEvent::SubagentStopped { .. } => {
            vec![MappedEvent::tui_and_observable()]
        }

        // ── Category ④: 观测层（broadcast 给外部监听器）─────────────────────────
        // 注：当前 ExecutorEvent 中无 AgentLifecycle/TurnCompleted 变体，
        // 此处预留路由位。未来扩展时启用。

        // ── Filtered: no output ───────────────────────────────────────────────────
        ExecutorEvent::LlmCallStart { .. }
        | ExecutorEvent::LlmCallEnd { usage: None, .. }
        | ExecutorEvent::LlmRequestPayload { .. } => {
            vec![MappedEvent::none()]
        }

        // ── Synthetic user message (Category ①) ─────────────────────────────────
        ExecutorEvent::MessageAdded(msg) => {
            let text = msg.content();
            vec![MappedEvent {
                updates: vec![SessionUpdate::UserMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text.to_string())),
                ))],
                forward_to_tui: false,
                hitl_pending: false,
                observable: false,
                source_agent_id: None,
            }]
        }
    }
}

/// Convert an [`ExecutorEvent`] to an [`AcpEvent`] DTO for the `peri/agent_event` channel.
///
/// Returns `None` for events that are already handled via `session/update` (Category ①)
/// or filtered out entirely.
pub fn executor_event_to_acp(event: &ExecutorEvent) -> Option<super::AcpEvent> {
    use super::AcpEvent;
    match event {
        ExecutorEvent::StateSnapshot(msgs) => {
            let messages_json = serde_json::to_string(msgs).ok()?;
            Some(AcpEvent::StateSnapshot { messages_json })
        }
        ExecutorEvent::TurnCommitted { messages, steps } => {
            let messages_json = serde_json::to_string(messages).ok()?;
            Some(AcpEvent::TurnCommitted {
                messages_json,
                steps: *steps,
            })
        }
        ExecutorEvent::StateSnapshotMeta {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
        } => Some(AcpEvent::StateSnapshotMeta {
            message_count: *message_count,
            total_tokens: *total_tokens,
            current_step: *current_step,
            consecutive_failures: *consecutive_failures,
            budget_pct: *budget_pct,
            context_total_tokens: *context_total_tokens,
        }),
        ExecutorEvent::SubagentStarted {
            agent_name,
            instance_id,
            is_background,
        } => Some(AcpEvent::SubagentStarted {
            agent_name: agent_name.clone(),
            instance_id: instance_id.clone(),
            is_background: *is_background,
        }),
        ExecutorEvent::SubagentStopped {
            agent_name,
            result,
            is_error,
            instance_id,
        } => Some(AcpEvent::SubagentStopped {
            agent_name: agent_name.clone(),
            result: result.clone(),
            is_error: *is_error,
            instance_id: instance_id.clone(),
        }),
        ExecutorEvent::CompactStarted => Some(AcpEvent::CompactStarted),
        ExecutorEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            messages,
        } => {
            let files_dto: Vec<crate::event::dto::CompactFileInfoDto> = files
                .iter()
                .map(|f| crate::event::dto::CompactFileInfoDto {
                    path: f.path.clone(),
                    lines: f.lines,
                })
                .collect();
            let messages_json = serde_json::to_string(messages).ok()?;
            Some(AcpEvent::CompactCompleted {
                summary: summary.clone(),
                files: files_dto,
                skills: skills.clone(),
                micro_cleared: *micro_cleared,
                messages_json,
            })
        }
        ExecutorEvent::CompactError { message } => Some(AcpEvent::CompactError {
            message: message.clone(),
        }),
        ExecutorEvent::RewindCompleted { summary, messages } => {
            let messages_json = serde_json::to_string(messages).ok()?;
            Some(AcpEvent::RewindCompleted {
                summary: summary.clone(),
                messages_json,
            })
        }
        ExecutorEvent::BackgroundTaskCompleted(result) => Some(AcpEvent::BackgroundTaskCompleted {
            task_id: result.task_id.clone(),
            agent_name: result.agent_name.clone(),
            success: result.success,
            output: result.output.clone(),
            tool_calls_count: result.tool_calls_count,
            duration_ms: result.duration_ms,
            child_thread_id: result.child_thread_id.clone(),
        }),
        ExecutorEvent::BgToolStep { child_thread_id } => Some(AcpEvent::BgToolStep {
            child_thread_id: child_thread_id.clone(),
        }),
        ExecutorEvent::LspDiagnostics {
            errors,
            warnings,
            files_with_errors,
        } => Some(AcpEvent::LspDiagnostics {
            errors: *errors,
            warnings: *warnings,
            files_with_errors: *files_with_errors,
        }),
        ExecutorEvent::AgentExecutionFailed { message } => Some(AcpEvent::AgentExecutionFailed {
            message: message.clone(),
        }),
        ExecutorEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        } => Some(AcpEvent::ContextWarning {
            used_tokens: *used_tokens,
            total_tokens: *total_tokens,
            percentage: *percentage,
        }),
        ExecutorEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => Some(AcpEvent::LlmRetrying {
            attempt: *attempt,
            max_attempts: *max_attempts,
            delay_ms: *delay_ms,
            error: error.clone(),
        }),
        ExecutorEvent::WorkflowProgress(payload) => Some(AcpEvent::WorkflowProgress {
            run_id: payload.run_id.clone(),
            workflow_name: payload.workflow_name.clone(),
            event_type: payload.event_type.clone(),
            agent_id: payload.agent_id,
            phase: payload.phase.clone(),
            label: payload.label.clone(),
            agent_status: payload.agent_status.clone(),
            token_count: payload.token_count,
            tool_count: payload.tool_count,
            run_status: payload.run_status.clone(),
            message: payload.message.clone(),
        }),
        // Category ① events: already handled via session/update
        // Filtered events: not forwarded
        // Note: MessageAdded is handled via session/update (above), not forwarded as AcpEvent.
        ExecutorEvent::TextChunk { .. }
        | ExecutorEvent::AiReasoning { .. }
        | ExecutorEvent::ToolStart { .. }
        | ExecutorEvent::ToolEnd { .. }
        | ExecutorEvent::TodoUpdate(_)
        | ExecutorEvent::LlmCallStart { .. }
        | ExecutorEvent::LlmCallEnd { .. }
        | ExecutorEvent::MessageAdded(_)
        | ExecutorEvent::LlmRequestPayload { .. } => None,
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

#[cfg(test)]
#[path = "mapper_test.rs"]
mod tests;
