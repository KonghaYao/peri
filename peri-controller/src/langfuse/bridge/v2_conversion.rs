//! v2 render/observe 事件到观测事件的纯转换，保持来源 agent 身份。

use super::UnifiedLangfuseEvent;
use peri_agent::agent::events::CompactTrigger;
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent};
use peri_agent::messages::BaseMessage;
use peri_model::TokenUsage;

impl UnifiedLangfuseEvent {
    /// 将 RenderEvent 转换为 UnifiedLangfuseEvent（v2 render 路径）。
    /// 无 Langfuse 映射的变体返回 `None`。
    pub fn from_render_event(ev: RenderEvent) -> Option<Self> {
        match ev {
            RenderEvent::TextChunk { chunk, .. } => Some(UnifiedLangfuseEvent::TextChunk { chunk }),
            RenderEvent::BudgetWarning {
                percentage,
                used_tokens,
                total_tokens,
                ..
            } => Some(UnifiedLangfuseEvent::BudgetWarning {
                percentage,
                used_tokens,
                total_tokens,
                threshold_label: "context_window".to_string(),
            }),
            RenderEvent::ToolStarted {
                agent_id,
                tool_call_id,
                name,
                input,
                ..
            } => Some(UnifiedLangfuseEvent::ToolStart {
                agent_id: agent_id.to_string(),
                tool_call_id,
                name,
                input,
            }),
            RenderEvent::ToolEnded {
                agent_id,
                tool_call_id,
                output,
                is_error,
                ..
            } => Some(UnifiedLangfuseEvent::ToolEnd {
                agent_id: agent_id.to_string(),
                tool_call_id,
                output,
                is_error,
            }),
            // 其余 RenderEvent 变体无 Langfuse 映射
            _ => None,
        }
    }

    /// 将 ObserveEvent 转换为 UnifiedLangfuseEvent（v2 observe 路径）。
    /// 无 Langfuse 映射的变体返回 `None`。
    pub fn from_observe_event(ev: ObserveEvent) -> Option<Self> {
        match ev {
            ObserveEvent::LlmCallStart {
                agent_id,
                step,
                messages,
                tools,
                ..
            } => {
                let msgs: Vec<BaseMessage> = (*messages).clone();
                Some(UnifiedLangfuseEvent::LlmCallStart {
                    agent_id: agent_id.to_string(),
                    step,
                    messages: msgs,
                    tools,
                })
            }
            ObserveEvent::LlmCallEnd {
                agent_id,
                step,
                model,
                output,
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                request_id,
                ..
            } => {
                let usage = TokenUsage {
                    input_tokens: input_tokens as u32,
                    output_tokens: output_tokens as u32,
                    cache_creation_input_tokens: cache_creation_input_tokens
                        .and_then(|tokens| tokens.try_into().ok()),
                    cache_read_input_tokens: cache_read_input_tokens
                        .and_then(|tokens| tokens.try_into().ok()),
                };
                Some(UnifiedLangfuseEvent::LlmCallEnd {
                    agent_id: agent_id.to_string(),
                    step,
                    model,
                    output,
                    usage: Some(usage),
                    request_id,
                })
            }
            ObserveEvent::LlmRequestPayload {
                agent_id,
                step,
                body,
                ..
            } => Some(UnifiedLangfuseEvent::LlmRequestPayload {
                agent_id: agent_id.to_string(),
                step,
                body,
            }),
            ObserveEvent::CompactStarted { strategy, .. } => {
                Some(UnifiedLangfuseEvent::CompactStarted {
                    strategy,
                    trigger: CompactTrigger::Auto, // v2 自动触发
                })
            }
            // S1.4：cancel 且未提交变更的 CompactEnded → 闭合 compact span。
            // 不携带 token 估算（无变更发生）；outcome 字段区分
            // Interrupted（取消未提交）与 MessagesCompacted 路径。
            ObserveEvent::CompactEnded { outcome, .. } => {
                Some(UnifiedLangfuseEvent::CompactEnded {
                    summary: String::new(),
                    files_count: 0,
                    skills_count: 0,
                    micro_cleared: 0,
                    is_error: false,
                    error_message: String::new(),
                    estimated_tokens_saved: 0,
                    estimated_tokens_before: 0,
                    estimated_tokens_after: 0,
                    cache_hit_rate_before: 0.0,
                    full_escalation_reason: None,
                    outcome: Some(format!("{:?}", outcome)),
                })
            }
            ObserveEvent::MessagesCompacted {
                summary,
                files,
                skills,
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason,
                outcome,
                ..
            } => Some(UnifiedLangfuseEvent::CompactEnded {
                summary,
                files_count: files.len(),
                skills_count: skills.len(),
                micro_cleared: 0, // v2 无此字段
                is_error: false,
                error_message: String::new(),
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason: full_escalation_reason.map(|r| format!("{:?}", r)),
                outcome: Some(format!("{:?}", outcome)),
            }),
            ObserveEvent::StageStarted {
                agent_id,
                stage,
                turn_id,
                ..
            } => Some(UnifiedLangfuseEvent::StageStarted {
                agent_id: agent_id.to_string(),
                stage,
                turn_id: turn_id.to_string(),
            }),
            ObserveEvent::StageEnded {
                agent_id, status, ..
            } => Some(UnifiedLangfuseEvent::StageEnded {
                agent_id: agent_id.to_string(),
                status,
            }),
            ObserveEvent::MessageQueueDrained {
                agent_id,
                prompt,
                defer,
                info,
                ..
            } => Some(UnifiedLangfuseEvent::MessageQueueDrained {
                agent_id: agent_id.to_string(),
                prompt,
                defer,
                info,
            }),
            ObserveEvent::AiReasoningChunk { text, .. } => {
                Some(UnifiedLangfuseEvent::AiReasoningChunk { text })
            }
            ObserveEvent::TurnError { reason, .. } => {
                Some(UnifiedLangfuseEvent::TurnError { reason })
            }
            // v2 SubagentStart/Stop → Unified（C4）：子 agent 生命周期事件直达
            ObserveEvent::SubagentStart {
                agent_id,
                child_agent_id,
                agent_name,
                is_background,
                ..
            } => Some(UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id: agent_id.to_string(),
                child_agent_id: child_agent_id.to_string(),
                agent_name,
                is_background,
            }),
            ObserveEvent::SubagentStop {
                agent_id,
                child_agent_id,
                agent_name,
                result,
                is_error,
                ..
            } => Some(UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id: agent_id.to_string(),
                child_agent_id: child_agent_id.to_string(),
                agent_name,
                result,
                is_error,
            }),
        }
    }
}
