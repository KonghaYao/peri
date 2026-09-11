//! v1 协议载体到观测事件的纯转换。

use super::UnifiedLangfuseEvent;
use crate::langfuse::tracer::stages::MAIN_AGENT_KEY;
use peri_agent::agent::events::ExecutorEvent;
use peri_agent::messages::BaseMessage;

impl UnifiedLangfuseEvent {
    /// 将 ExecutorEvent 转换为 UnifiedLangfuseEvent（v1 路径）。
    /// 无 Langfuse 映射的变体返回 `None`。
    pub fn from_executor_event(ev: ExecutorEvent) -> Option<Self> {
        match ev {
            ExecutorEvent::LlmCallStart {
                step,
                messages,
                tools,
            } => {
                let msgs: Vec<BaseMessage> = (*messages).clone();
                Some(UnifiedLangfuseEvent::LlmCallStart {
                    // v1 ExecutorEvent 无 agent_id（v2 ObserveEvent 才携带）：
                    // workflow agent 事件固定归属主 agent slot。
                    agent_id: MAIN_AGENT_KEY.to_string(),
                    step,
                    messages: msgs,
                    tools,
                })
            }
            ExecutorEvent::LlmRequestPayload { step, body } => {
                Some(UnifiedLangfuseEvent::LlmRequestPayload {
                    agent_id: MAIN_AGENT_KEY.to_string(),
                    step,
                    body,
                })
            }
            ExecutorEvent::LlmCallEnd {
                step,
                model,
                output,
                usage,
                request_id,
                ..
            } => Some(UnifiedLangfuseEvent::LlmCallEnd {
                agent_id: MAIN_AGENT_KEY.to_string(),
                step,
                model,
                output,
                usage,
                request_id,
            }),
            ExecutorEvent::LlmRetrying {
                attempt,
                max_attempts,
                delay_ms,
                error,
            } => Some(UnifiedLangfuseEvent::LlmRetrying {
                attempt,
                max_attempts,
                delay_ms,
                error,
            }),
            ExecutorEvent::TextChunk { chunk, .. } => {
                Some(UnifiedLangfuseEvent::TextChunk { chunk })
            }
            ExecutorEvent::ToolStart {
                tool_call_id,
                name,
                input,
                source_agent_id,
                ..
            } => Some(UnifiedLangfuseEvent::ToolStart {
                // v1 事件若无 source_agent_id 则归属主 agent slot
                agent_id: source_agent_id.unwrap_or_else(|| MAIN_AGENT_KEY.to_string()),
                tool_call_id,
                name,
                input,
            }),
            ExecutorEvent::ToolEnd {
                tool_call_id,
                output,
                is_error,
                source_agent_id,
                ..
            } => Some(UnifiedLangfuseEvent::ToolEnd {
                agent_id: source_agent_id.unwrap_or_else(|| MAIN_AGENT_KEY.to_string()),
                tool_call_id,
                output,
                is_error,
            }),
            ExecutorEvent::CompactStarted {
                strategy, trigger, ..
            } => Some(UnifiedLangfuseEvent::CompactStarted { strategy, trigger }),
            ExecutorEvent::CompactCompleted {
                summary,
                files,
                skills,
                affected_count,
                estimated_tokens_saved,
                ..
            } => Some(UnifiedLangfuseEvent::CompactEnded {
                summary,
                files_count: files.len(),
                skills_count: skills.len(),
                micro_cleared: affected_count,
                is_error: false,
                error_message: String::new(),
                estimated_tokens_saved,
                estimated_tokens_before: 0,
                estimated_tokens_after: 0,
                cache_hit_rate_before: 0.0,
                full_escalation_reason: None,
                outcome: None,
            }),
            ExecutorEvent::SessionStarted { frozen_summary, .. } => {
                Some(UnifiedLangfuseEvent::SessionStarted { frozen_summary })
            }
            ExecutorEvent::MiddlewareStarted { mw_name, hook, .. } => {
                Some(UnifiedLangfuseEvent::MiddlewareStarted { mw_name, hook })
            }
            ExecutorEvent::MiddlewareEnded {
                mw_name,
                hook,
                status,
                error,
                ..
            } => Some(UnifiedLangfuseEvent::MiddlewareEnded {
                mw_name,
                hook,
                status,
                error,
            }),
            ExecutorEvent::BudgetThresholdHit {
                threshold,
                current_pct,
                tokens_in,
                tokens_out,
                ..
            } => Some(UnifiedLangfuseEvent::BudgetWarning {
                percentage: current_pct,
                used_tokens: tokens_in,
                total_tokens: tokens_out,
                threshold_label: format!("{:?}", threshold),
            }),
            ExecutorEvent::WorkflowStarted {
                workflow_id,
                plan_summary,
                ..
            } => Some(UnifiedLangfuseEvent::WorkflowStarted {
                workflow_id,
                plan_summary,
            }),
            ExecutorEvent::WorkflowEnded {
                workflow_id,
                agents_spawned,
                tool_calls,
                ..
            } => Some(UnifiedLangfuseEvent::WorkflowEnded {
                workflow_id,
                agents_spawned,
                tool_calls,
            }),
            // 无 Langfuse 映射的事件
            ExecutorEvent::TurnStarted { .. }
            | ExecutorEvent::UserInputRunStarted { .. }
            | ExecutorEvent::UserInputQueueChanged(_)
            | ExecutorEvent::UserInputDelivered { .. }
            | ExecutorEvent::GoalSnapshot { .. }
            | ExecutorEvent::TurnEnded { .. }
            | ExecutorEvent::StateSnapshotMeta { .. }
            | ExecutorEvent::SubagentStarted { .. }
            | ExecutorEvent::SubagentStopped { .. }
            | ExecutorEvent::BackgroundTaskCompleted(_)
            | ExecutorEvent::MessageAdded(_)
            | ExecutorEvent::StateSnapshot(_)
            | ExecutorEvent::TurnCommitted { .. }
            | ExecutorEvent::AiReasoning { .. }
            | ExecutorEvent::ContextWarning { .. }
            | ExecutorEvent::RewindCompleted { .. }
            | ExecutorEvent::TodoUpdate(_)
            | ExecutorEvent::LspDiagnostics { .. }
            | ExecutorEvent::BgToolStep { .. }
            | ExecutorEvent::WorkflowProgress(_)
            | ExecutorEvent::AgentExecutionFailed { .. }
            | ExecutorEvent::TurnSuspended { .. }
            | ExecutorEvent::SystemReminder(_)
            | ExecutorEvent::SystemNotification { .. }
            | ExecutorEvent::OauthNeeded { .. }
            | ExecutorEvent::OauthCompleted { .. }
            | ExecutorEvent::OauthFailed { .. }
            | ExecutorEvent::BgRegistryEvent(_)
            | ExecutorEvent::CommandFeedback(_) => None,
        }
    }
}
