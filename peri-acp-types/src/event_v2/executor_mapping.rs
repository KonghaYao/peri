//! v2 → ExecutorEvent 的协议兼容转换，穷尽声明每个变体的映射或过滤。
//!
//! chunk 透传消息级 message_id；工具事件从 turn_id 派生 message_id。
//! 共享转换不把 agent_id 解释为子 Agent 来源，source_agent_id 保持 None；
//! root 身份由 forwarder 的 envelope 传递，子 Agent 转发器显式注入来源。
//! SubagentStart/Stop 的 child_agent_id 透传为 instance_id。

use super::{ObserveEvent, RenderEvent, StateEvent};
use crate::{
    event::{CompactTrigger, ExecutorEvent},
    messages::MessageId,
};

/// 将 v2 `RenderEvent` 转换为 0 或 1 个 `ExecutorEvent`（穷尽匹配）。
pub fn render_event_to_executor(event: RenderEvent) -> Option<ExecutorEvent> {
    match event {
        RenderEvent::TextChunk {
            message_id, chunk, ..
        } => Some(ExecutorEvent::TextChunk {
            // v2 chunk 事件携带消息级身份（每次 LLM 调用一个稳定 message_id），
            // 不再用 turn_id 填充——同一 turn 多次迭代的消息各自独立（ACP 标准
            // messageId 语义：变化 = 新消息）。
            message_id,
            chunk,
            source_agent_id: None,
        }),
        RenderEvent::ThinkingChunk {
            message_id, chunk, ..
        } => Some(ExecutorEvent::AiReasoning {
            message_id,
            text: chunk,
            source_agent_id: None,
        }),
        RenderEvent::ToolStarted {
            turn_id,
            tool_call_id,
            name,
            input,
            ..
        } => Some(ExecutorEvent::ToolStart {
            message_id: MessageId::from(turn_id.as_uuid()),
            tool_call_id,
            name,
            input,
            source_agent_id: None,
        }),
        RenderEvent::ToolEnded {
            turn_id,
            tool_call_id,
            name,
            output,
            is_error,
            ..
        } => Some(ExecutorEvent::ToolEnd {
            message_id: MessageId::from(turn_id.as_uuid()),
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
        // HitlPending：v1 中无对应变体，由 HITL 审批独立通道（RequestPermission）
        // 处理，不在事件链映射。
        RenderEvent::HitlPending { .. } => None,
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

/// 将 v2 `StateEvent` 转换为 `ExecutorEvent`（穷尽匹配）。
pub fn state_event_to_executor(event: StateEvent) -> Option<ExecutorEvent> {
    match event {
        StateEvent::ProtocolEvent { event, .. } => Some(event),
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
        StateEvent::GoalSnapshot {
            objective,
            status,
            token_budget,
            tokens_used,
            time_used_seconds,
            continuation_count,
            blocked_reason,
            ..
        } => Some(ExecutorEvent::GoalSnapshot {
            objective,
            status,
            token_budget,
            tokens_used,
            time_used_seconds,
            continuation_count,
            blocked_reason,
        }),
        StateEvent::SyntheticUserMessage { text, .. } => Some(ExecutorEvent::MessageAdded(
            crate::messages::BaseMessage::human(crate::messages::MessageContent::text(text)),
        )),
        // TurnSuspended：TUI 挂起信号（归档 current_turn + 停止 loading），
        // 经 ExecutorEvent::TurnSuspended 透传 turn_id/agent_id 身份。
        StateEvent::TurnSuspended { turn_id, agent_id } => Some(ExecutorEvent::TurnSuspended {
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
        }),
    }
}

/// 将 v2 `ObserveEvent` 转换为 `ExecutorEvent`（穷尽匹配）。
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
                cache_creation_input_tokens: cache_creation_input_tokens
                    .and_then(|tokens| tokens.try_into().ok()),
                cache_read_input_tokens: cache_read_input_tokens
                    .and_then(|tokens| tokens.try_into().ok()),
            }),
            stop_reason: None,
            request_id,
            source_agent_id: None,
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
        // CompactEnded：无变更的结束路径（cancel 且未提交变更），v1 无对应
        // 事件变体；仅 Langfuse bridge 直消费 v2 闭合 span。
        ObserveEvent::CompactEnded { .. } => None,
        ObserveEvent::MessagesCompacted {
            summary,
            messages,
            files,
            skills,
            strategy,
            affected_count,
            estimated_tokens_saved,
            ..
        } => Some(ExecutorEvent::CompactCompleted {
            summary,
            messages,
            trigger: CompactTrigger::Auto,
            strategy,
            affected_count,
            estimated_tokens_saved,
            files,
            skills,
        }),
        // TurnError：TUI 错误展示经 executor_helpers 的 AgentExecutionFailed
        // （LoopResult::Error 分支）；Langfuse 经 bridge 直消费 v2。v1 无对应变体。
        ObserveEvent::TurnError { .. } => None,
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
        // ── tracer-only：Langfuse bridge 直消费 v2，v1 无对应变体 ──
        ObserveEvent::AiReasoningChunk { .. } => None,
        ObserveEvent::StageStarted { .. } => None,
        ObserveEvent::StageEnded { .. } => None,
        ObserveEvent::MessageQueueDrained { .. } => None,
    }
}

#[cfg(test)]
#[path = "executor_mapping_test.rs"]
mod tests;
