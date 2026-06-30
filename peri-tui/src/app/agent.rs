// ── Live code retained after unified event mapping ──
// map_acp_event: handles categories ②③ only
// Category ① events (TextChunk) now come through session/update → handle_session_update_peri()
// Note: AiReasoning removed — reasoning rendered via state machine streaming_reasoning bridge (P5)

use peri_acp::event::AcpEvent;

pub use super::provider::LlmProvider;
use super::AgentEvent;

/// 将 AcpEvent DTO 映射为 TUI AgentEvent。
///
/// 仅处理 session/update 无法映射的事件（类别②③）。
/// 类别①事件（TextChunk, ToolStart, ToolEnd, TodoUpdate）
/// 已通过 session/update → handle_session_update_peri() 处理，此处返回 None。
pub(crate) fn map_acp_event(event: AcpEvent, _cwd: &str) -> Option<AgentEvent> {
    Some(match event {
        // ── 类别③：无 SessionUpdate 映射，仍通过 peri/agent_event ──
        AcpEvent::StateSnapshot { messages_json } => {
            let msgs: Vec<peri_agent::messages::BaseMessage> =
                serde_json::from_str(&messages_json).unwrap_or_default();
            AgentEvent::StateSnapshot(msgs)
        }
        AcpEvent::TurnCommitted {
            messages_json,
            steps,
        } => {
            let msgs: Vec<peri_agent::messages::BaseMessage> =
                serde_json::from_str(&messages_json).unwrap_or_default();
            AgentEvent::TurnCommitted {
                messages: msgs,
                steps,
            }
        }
        AcpEvent::StateSnapshotMeta {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
        } => AgentEvent::StateSnapshotMeta {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
        },
        AcpEvent::SubagentStarted {
            agent_name,
            instance_id,
            is_background,
        } => AgentEvent::SubAgentStart {
            agent_id: agent_name.clone(),
            instance_id,
            task_preview: String::new(),
            is_background,
        },
        AcpEvent::SubagentStopped {
            agent_name,
            result,
            is_error,
            instance_id,
        } => AgentEvent::SubAgentEnd {
            agent_id: Some(agent_name),
            instance_id: Some(instance_id),
            result,
            is_error,
        },
        AcpEvent::CompactStarted => AgentEvent::CompactStarted,
        AcpEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            messages_json,
        } => {
            let messages: Vec<peri_agent::messages::BaseMessage> =
                serde_json::from_str(&messages_json).unwrap_or_default();
            AgentEvent::CompactCompleted {
                summary,
                files,
                skills,
                micro_cleared,
                messages,
            }
        }
        AcpEvent::CompactError { message } => AgentEvent::CompactError(message),
        AcpEvent::RewindCompleted {
            summary,
            messages_json,
        } => {
            let messages: Vec<peri_agent::messages::BaseMessage> =
                serde_json::from_str(&messages_json).unwrap_or_default();
            AgentEvent::RewindCompleted { summary, messages }
        }
        AcpEvent::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        } => AgentEvent::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        },
        AcpEvent::BgToolStep { child_thread_id } => AgentEvent::BgToolStep { child_thread_id },
        AcpEvent::LspDiagnostics {
            errors,
            warnings,
            files_with_errors,
        } => AgentEvent::LspDiagnostics {
            errors,
            warnings,
            files_with_errors,
        },
        AcpEvent::AgentExecutionFailed { message } => {
            if message == "Interrupted by user" {
                AgentEvent::Interrupted
            } else {
                AgentEvent::Error(message)
            }
        }
        AcpEvent::WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            agent_id,
            phase,
            label,
            agent_status,
            token_count,
            tool_count,
            run_status,
            message,
        } => AgentEvent::WorkflowProgress(peri_acp::event::WorkflowProgressDto {
            run_id,
            workflow_name,
            event_type,
            agent_id,
            phase,
            label,
            agent_status,
            token_count,
            tool_count,
            run_status,
            message,
        }),

        // ── 类别②：SessionUpdate 丢失信息的增强事件 ──
        AcpEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        } => AgentEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        },
        AcpEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => AgentEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        },
    })
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;
