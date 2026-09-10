//! Agent-event DTO conversion; no atom publication.

use crate::kit::acp_types::{AcpEventData, FeedbackChannel, FeedbackLevel, TuiCommandFeedback};
use peri_acp::event::AcpEvent;
use peri_acp_types::event_data::{OauthNeeded, SystemNotification};
use tracing::debug;

/// 将 `peri/agent_event` 通道的 `AcpEvent` DTO 转换为 kit 层的 `AcpEventData`。
///
/// 当前映射列表（需与后续 S5+ 迭代同步扩展）：
/// - `SubagentStarted` / `SubagentStopped` / `LlmRetrying` → 对应的 `AcpEventData` 变体
/// - 其他变体返回 `None`（不存在对应的 `AcpEventData` 或以其他通道覆盖）
pub(super) fn decode_agent_event(event: AcpEvent) -> Option<AcpEventData> {
    match event {
        AcpEvent::SubagentStarted {
            agent_name,
            instance_id,
            is_background,
        } => Some(AcpEventData::SubagentStarted {
            agent_id: instance_id,
            agent_name,
            is_background,
        }),
        AcpEvent::SubagentStopped {
            instance_id,
            result,
            is_error,
            ..
        } => Some(AcpEventData::SubagentStopped {
            agent_id: instance_id,
            result,
            is_error,
        }),
        // ── §4.8 Agent Event Extensions (P1-5) ──
        AcpEvent::TurnCommitted {
            messages_json,
            steps,
        } => Some(AcpEventData::TurnCommitted {
            messages_json,
            steps,
        }),
        AcpEvent::CompactStarted => Some(AcpEventData::CompactStarted),
        AcpEvent::CompactCompleted {
            summary,
            messages_json,
            trigger,
            strategy,
            affected_count,
            estimated_tokens_saved,
            files,
            skills,
        } => Some(AcpEventData::CompactCompleted {
            summary,
            messages_json,
            trigger,
            strategy,
            affected_count,
            estimated_tokens_saved,
            files,
            skills,
        }),
        AcpEvent::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        } => Some(AcpEventData::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        }),
        AcpEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => Some(AcpEventData::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        }),
        AcpEvent::AgentExecutionFailed { message } => {
            Some(AcpEventData::AgentExecutionFailed { message })
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
        } => Some(AcpEventData::WorkflowProgress {
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
        AcpEvent::RewindCompleted {
            messages_json,
            summary: _,
        } => Some(AcpEventData::RewindCompleted { messages_json }),
        // SystemNotification：MCP 上下线等连接状态变化（peri/agent_event 通道
        // 送达），转换为 AcpEventData::SystemNotification 显示系统通知。
        AcpEvent::SystemNotification { text, level } => {
            Some(AcpEventData::SystemNotification(SystemNotification {
                text,
                level,
            }))
        }
        // CommandFeedback：命令执行反馈（Phase 3 事件链路，经 peri/agent_event
        // 通道送达，无标准 SessionUpdate）。level/channel 为 wire string 化
        // camelCase（"info" / "uiOnly"），解析为结构化枚举后推入 dual-bridge。
        // 未知 level 回落 Info、未知 channel 回落 UiOnly（Phase 1 缺省语义）。
        AcpEvent::CommandFeedback {
            level,
            message,
            channel,
        } => Some(AcpEventData::CommandFeedback(TuiCommandFeedback {
            level: match level.as_str() {
                "warning" => FeedbackLevel::Warning,
                "error" => FeedbackLevel::Error,
                _ => FeedbackLevel::Info,
            },
            message,
            channel: match channel.as_str() {
                "session" => FeedbackChannel::Session,
                _ => FeedbackChannel::UiOnly,
            },
        })),
        // TurnSuspended：bg agent/cron/workflow 挂起信号——归档 current_turn、
        // 停止 loading spinner。双轨下线后（2026-08-05-3.0-m-event-chain-canonical）
        // 此信号仅经 ACP peri/agent_event 通道送达。
        AcpEvent::TurnSuspended { .. } => Some(AcpEventData::TurnSuspended),
        // OAuth 授权事件（host 级，跨 session）：OauthNeeded 打开 popup 收集
        // 授权码，Completed/Failed 关闭 popup 并提示结果。
        AcpEvent::OauthNeeded {
            server_name,
            auth_url,
        } => Some(AcpEventData::OauthNeeded(OauthNeeded {
            server_name,
            auth_url,
        })),
        AcpEvent::OauthCompleted { server_name } => {
            Some(AcpEventData::OauthCompleted { server_name })
        }
        AcpEvent::OauthFailed { server_name, error } => {
            Some(AcpEventData::OauthFailed { server_name, error })
        }
        AcpEvent::OauthRestored { server_name } => {
            Some(AcpEventData::OauthRestored { server_name })
        }
        AcpEvent::GoalSnapshot {
            objective,
            status,
            token_budget,
            tokens_used,
            time_used_seconds,
            continuation_count,
            blocked_reason,
        } => Some(AcpEventData::GoalSnapshot {
            objective,
            status,
            token_budget,
            tokens_used,
            time_used_seconds,
            continuation_count,
            blocked_reason,
        }),
        _ => {
            debug!("kit ACP notifier: AcpEvent variant not yet mapped to AcpEventData, dropping");
            None
        }
    }
}
