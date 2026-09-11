//! TUI legacy AcpEvent 投影与发送；调用方完成 agent_event capability 门控。

use super::TransportEventSink;
use crate::event::AcpEvent;
use peri_acp_types::event::ExecutorEvent;
use serde_json::json;
use tracing::error;

/// Serializes a serde `Serialize` value into its string wire representation.
/// The output follows the input type's serde rename form: CompactStrategy/
/// CompactOutcome produce snake_case, CommandFeedback level/channel produce
/// camelCase (`"info"`/`"uiOnly"`); TUI string matching relies on these forms.
fn to_serde_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

impl TransportEventSink {
    pub(super) async fn push_legacy_event(&self, session_id: &str, event: &ExecutorEvent) {
        let acp_event = match event {
            ExecutorEvent::UserInputRunStarted {
                generation,
                request_id,
            } => Some(AcpEvent::UserInputRunStarted {
                generation: generation.clone(),
                request_id: request_id.clone(),
            }),
            ExecutorEvent::UserInputQueueChanged(snapshot) => {
                Some(AcpEvent::UserInputQueueChanged {
                    snapshot: snapshot.clone(),
                })
            }
            ExecutorEvent::UserInputDelivered {
                input_id,
                generation,
                content,
            } => Some(AcpEvent::UserInputDelivered {
                input_id: input_id.clone(),
                generation: generation.clone(),
                content: content.clone(),
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
            ExecutorEvent::CompactStarted { .. } => Some(AcpEvent::CompactStarted),
            ExecutorEvent::CompactCompleted {
                summary,
                messages,
                trigger,
                strategy,
                affected_count,
                estimated_tokens_saved,
                files,
                skills,
            } => {
                let messages_json = match serde_json::to_string(messages) {
                    Ok(json) => json,
                    Err(e) => {
                        error!(error = %e, "EventSink: serialize CompactCompleted messages failed");
                        return;
                    }
                };
                Some(AcpEvent::CompactCompleted {
                    summary: summary.clone(),
                    messages_json,
                    trigger: to_serde_str(trigger),
                    strategy: to_serde_str(strategy),
                    affected_count: *affected_count,
                    estimated_tokens_saved: *estimated_tokens_saved,
                    files: files
                        .iter()
                        .map(|file| crate::event::CompactFileInfoDto {
                            path: file.path.clone(),
                            lines: file.lines,
                        })
                        .collect(),
                    skills: skills.clone(),
                })
            }
            ExecutorEvent::AgentExecutionFailed { message } => {
                Some(AcpEvent::AgentExecutionFailed {
                    message: message.clone(),
                })
            }
            // Rewind v2：RewindCompleted 经 peri/agent_event 通道送达 TUI，
            // TUI 侧 acp_notifier 转换为 AcpEventData::RewindCompleted 驱动
            // 弹窗关闭 + 消息区重建 + 输入框回填。
            ExecutorEvent::RewindCompleted { summary, messages } => {
                let messages_json = match serde_json::to_string(messages) {
                    Ok(json) => json,
                    Err(e) => {
                        error!(error = %e, "EventSink: serialize RewindCompleted messages failed");
                        return;
                    }
                };
                Some(AcpEvent::RewindCompleted {
                    summary: summary.clone(),
                    messages_json,
                })
            }
            // SystemNotification：MCP 上下线等连接状态变化经 peri/agent_event
            // 通道送达 TUI（AcpEventData::SystemNotification → system-notification
            // 通知显示）。
            ExecutorEvent::SystemNotification { text, level } => {
                Some(AcpEvent::SystemNotification {
                    text: text.clone(),
                    level: level.clone(),
                })
            }
            // OAuth：MCP 授权流程事件经 host 装配面回调产生（初始化/重连
            // 阶段无 session event_sink），此处分支覆盖运行中经 session 链
            // 转发的场景；初始化阶段由 host 级通道（oauth_event_tx）直达。
            ExecutorEvent::OauthNeeded {
                server_name,
                auth_url,
            } => Some(AcpEvent::OauthNeeded {
                server_name: server_name.clone(),
                auth_url: auth_url.clone(),
            }),
            ExecutorEvent::OauthCompleted { server_name } => Some(AcpEvent::OauthCompleted {
                server_name: server_name.clone(),
            }),
            ExecutorEvent::OauthFailed { server_name, error } => Some(AcpEvent::OauthFailed {
                server_name: server_name.clone(),
                error: error.clone(),
            }),
            // TurnSuspended：TUI 挂起信号（归档 current_turn + 停止 loading）。
            // v2 StateEvent::TurnSuspended 经 v1 兼容映射（events_v2::
            // state_event_to_executor）到达此处；双轨下线（2026-08-05-3.0-m-
            // event-chain-canonical）后此信号仅经 ACP 路径送达 TUI。
            ExecutorEvent::TurnSuspended { turn_id, agent_id } => Some(AcpEvent::TurnSuspended {
                turn_id: turn_id.clone(),
                agent_id: agent_id.clone(),
            }),
            // StateSnapshotMeta：状态栏上下文消耗（budget_pct + 总量）。
            // v2 StateEvent::StateSnapshot 经 mapper_v2 → v1 StateSnapshotMeta
            // 到达此处；双轨下线（v2_bridge.rs 删除）后此信号仅经 ACP 路径
            // 送达 TUI（acp_notifier.rs 写 CONTEXT_USAGE atom）。此前该分支
            // 缺失落入 `_ => None` 静默丢弃，TUI status_bar ctx% 段永不渲染
            // （e2e compact-command 回归，2026-08-06 修复）。
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
            ExecutorEvent::GoalSnapshot {
                objective,
                status,
                token_budget,
                tokens_used,
                time_used_seconds,
                continuation_count,
                blocked_reason,
            } => Some(AcpEvent::GoalSnapshot {
                objective: objective.clone(),
                status: *status,
                token_budget: *token_budget,
                tokens_used: *tokens_used,
                time_used_seconds: *time_used_seconds,
                continuation_count: *continuation_count,
                blocked_reason: blocked_reason.clone(),
            }),
            // TurnCommitted：messages 载荷（全量消息快照）在本链路无消费者——
            // TUI 仅用 steps 做 ReAct 迭代边界刷新检查点（acp_events/mod.rs:331
            // 丢弃 messages_json），Langfuse bridge 亦不读取（bridge.rs:319）。
            // 序列化该载荷是纯浪费；`{ .. }` 通配字段绑定，兼容 peri-agent 侧
            // messages 改 Arc<Vec<BaseMessage>> 传递，本分支无需再改。
            ExecutorEvent::TurnCommitted { .. } => None,
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
            // CommandFeedback：命令执行反馈经 peri/agent_event 通道送达 TUI
            // 通知条（level/channel 复用 to_serde_str 先例）；channel=session
            // 由 TUI 侧 opt-in 另写系统消息（Phase 4 落 TUI 本地拦截）。
            ExecutorEvent::CommandFeedback(fb) => Some(AcpEvent::CommandFeedback {
                level: to_serde_str(&fb.level),
                message: fb.message.clone(),
                channel: to_serde_str(&fb.channel),
            }),
            _ => None,
        };
        if let Some(acp_event) = acp_event {
            let event_json = match serde_json::to_string(&acp_event) {
                Ok(json) => json,
                Err(e) => {
                    error!(error = %e, "EventSink: serialize AcpEvent failed");
                    return;
                }
            };
            let _ = self
                .transport
                .send_notification(
                    "peri/agent_event",
                    json!({
                        "sessionId": session_id,
                        "event_json": event_json,
                    }),
                )
                .await;
        }
    }
}
