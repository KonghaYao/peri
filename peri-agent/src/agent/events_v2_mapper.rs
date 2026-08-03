//! v2 事件 → v1 ExecutorEvent 桥接
//!
//! v2 stages 通过 `EventBus` 发出 `RenderEvent` / `StateEvent` / `ObserveEvent`，
//! 而 TUI 当前消费 `peri_agent::agent::events::ExecutorEvent`（即 ExecutorEvent）。
//! 本模块提供转换函数，让 v2 stages 驱动的循环能复用现有 TUI 管线，无需重写 UI 层。
//!
//! ## 设计原则
//!
//! - **无状态**：每个函数纯映射，不持有上下文
//! - **丢失语义可接受**：v2 事件携带的 `turn_id` / `agent_id` 在 v1 中无对应字段，
//!   除非 source_agent_id 不同于主 agent（SubAgent 路由），否则忽略
//! - **不存在的方向**：v1 → v2 不需要（v1 是被替换方）

use crate::agent::events::{CompactTrigger, ExecutorEvent};
use crate::agent::events_v2::{ObserveEvent, RenderEvent, StateEvent};

/// 将 v2 `RenderEvent` 转换为 0 或 1 个 `ExecutorEvent`
///
/// 返回 `Ok(None)` 表示该事件在 v1 中被过滤（如 HitlPending 保留位）。
pub fn render_event_to_executor(event: RenderEvent) -> Option<ExecutorEvent> {
    match event {
        RenderEvent::TextChunk { chunk, .. } => {
            // v2 不携带 message_id（turn_id 不等于 message_id），用 default
            Some(ExecutorEvent::TextChunk {
                message_id: Default::default(),
                chunk,
                source_agent_id: None,
            })
        }
        RenderEvent::ThinkingChunk { chunk, .. } => Some(ExecutorEvent::AiReasoning {
            text: chunk,
            source_agent_id: None,
        }),
        RenderEvent::ToolStarted {
            tool_call_id,
            name,
            input,
            ..
        } => Some(ExecutorEvent::ToolStart {
            message_id: Default::default(),
            tool_call_id,
            name,
            input,
            source_agent_id: None,
        }),
        RenderEvent::ToolEnded {
            tool_call_id,
            name,
            output,
            is_error,
            ..
        } => Some(ExecutorEvent::ToolEnd {
            message_id: Default::default(),
            tool_call_id,
            name,
            output,
            is_error,
            source_agent_id: None,
        }),
        RenderEvent::BudgetWarning {
            used_tokens,
            total_tokens,
            percentage,
            ..
        } => Some(ExecutorEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        }),
        RenderEvent::HitlPending { .. } => {
            // v1 中无 HitlPending 变体；保留位，由 HITL 审批独立通道处理
            None
        }
        RenderEvent::TurnCompleted {
            finalized_messages,
            steps,
            ..
        } => Some(ExecutorEvent::TurnCommitted {
            // Arc 直接透传（浅拷贝），消除每迭代的全量消息深拷贝
            messages: finalized_messages,
            steps,
        }),
    }
}

/// 将 v2 `StateEvent` 转换为 `ExecutorEvent`
pub fn state_event_to_executor(event: StateEvent) -> Option<ExecutorEvent> {
    match event {
        StateEvent::StateSnapshot {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
            ..
        } => Some(ExecutorEvent::StateSnapshotMeta {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
        }),
        StateEvent::SyntheticUserMessage { text, .. } => Some(ExecutorEvent::MessageAdded(
            crate::messages::BaseMessage::human(crate::messages::MessageContent::text(text)),
        )),
        _ => None,
    }
}

/// 将 v2 `ObserveEvent` 转换为 `ExecutorEvent`
pub fn observe_event_to_executor(event: ObserveEvent) -> Option<ExecutorEvent> {
    match event {
        ObserveEvent::LlmCallStart {
            step,
            messages,
            tools,
            ..
        } => Some(ExecutorEvent::LlmCallStart {
            step,
            messages,
            tools,
        }),
        ObserveEvent::LlmCallEnd {
            step,
            model,
            output,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            request_id,
            ..
        } => Some(ExecutorEvent::LlmCallEnd {
            step,
            model,
            output,
            usage: Some(peri_model::TokenUsage {
                input_tokens: input_tokens as u32,
                output_tokens: output_tokens as u32,
                // 0 表示 Provider 不支持 caching；保留 Option 让下游区分"不支持" vs "未命中"
                cache_creation_input_tokens: if cache_creation_input_tokens > 0 {
                    Some(cache_creation_input_tokens as u32)
                } else {
                    None
                },
                cache_read_input_tokens: if cache_read_input_tokens > 0 {
                    Some(cache_read_input_tokens as u32)
                } else {
                    None
                },
            }),
            stop_reason: None,
            request_id,
        }),
        ObserveEvent::CompactStarted {
            turn_id,
            agent_id,
            step,
            strategy,
            ..
        } => Some(ExecutorEvent::CompactStarted {
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
            step,
            strategy,
            trigger: CompactTrigger::Auto,
        }),
        ObserveEvent::MessagesCompacted {
            before_count,
            after_count,
            summary,
            messages,
            files,
            skills,
            strategy,
            affected_count,
            estimated_tokens_saved,
            estimated_tokens_before,
            estimated_tokens_after,
            changed_messages,
            changed_fields,
            no_op_candidates,
            full_escalation_reason,
            cache_hit_rate_before,
            outcome,
            ..
        } => Some(ExecutorEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared: before_count.saturating_sub(after_count),
            messages,
            token_before: estimated_tokens_before,
            token_after: estimated_tokens_after,
            strategy,
            affected_count,
            estimated_tokens_saved,
            estimated_tokens_before,
            estimated_tokens_after,
            changed_messages,
            changed_fields,
            no_op_candidates,
            full_escalation_reason,
            cache_hit_rate_before,
            outcome,
        }),
        ObserveEvent::SubagentStart {
            agent_name,
            child_agent_id,
            is_background,
            ..
        } => Some(ExecutorEvent::SubagentStarted {
            agent_name,
            instance_id: child_agent_id.to_string(),
            is_background,
        }),
        ObserveEvent::SubagentStop {
            agent_name,
            child_agent_id,
            result,
            is_error,
            ..
        } => Some(ExecutorEvent::SubagentStopped {
            agent_name,
            result,
            is_error,
            instance_id: child_agent_id.to_string(),
        }),
        ObserveEvent::LlmRequestPayload { step, body, .. } => {
            Some(ExecutorEvent::LlmRequestPayload { step, body })
        }
        _ => None,
    }
}

/// 统一的事件包装：把任意 v2 事件转为 ExecutorEvent
#[derive(Debug, Clone)]
pub enum V2Event {
    Render(RenderEvent),
    State(StateEvent),
    Observe(ObserveEvent),
}

impl V2Event {
    pub fn from_render(e: RenderEvent) -> Self {
        Self::Render(e)
    }
    pub fn from_state(e: StateEvent) -> Self {
        Self::State(e)
    }
    pub fn from_observe(e: ObserveEvent) -> Self {
        Self::Observe(e)
    }
}

impl From<V2Event> for Option<ExecutorEvent> {
    fn from(value: V2Event) -> Self {
        match value {
            V2Event::Render(e) => render_event_to_executor(e),
            V2Event::State(e) => state_event_to_executor(e),
            V2Event::Observe(e) => observe_event_to_executor(e),
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "events_v2_mapper_test.rs"]
mod tests;
