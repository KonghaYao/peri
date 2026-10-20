//! 终态错误 → goal 恢复尝试的稳定分类。
//!
//! 只有 `run_react_loop` 返回 `LoopResult::Error` 时才需要该分类：普通工具
//! 失败会作为工具结果返回模型，由 GoalMiddleware 的常规 steering 继续。
//! 分类结果是短标签，进入恢复提醒与日志，不把 provider 原文写进 prompt。

use std::sync::Arc;

use peri_acp_types::error::AgentError;

use super::GoalController;

/// 返回可恢复错误的稳定标签；`None` 表示该错误不应触发 goal 自动恢复。
///
/// 排除项：
/// - `Interrupted`：用户显式取消；
/// - `WorkBudgetExhausted`：预算耗尽需要显式授权，自动重试会绕过该契约；
/// - `NotAvailableWithoutTurn` / `SerializationError`：确定性的调用形状错误；
/// - `CompactNoLlm` / `CompactSummaryBudgetInvalid` / `CompactBudgetUnrecovered`：
///   确定性配置或预算错误，重试不会改变结果。
pub fn goal_recovery_reason(error: &AgentError) -> Option<&'static str> {
    match error {
        AgentError::Interrupted
        | AgentError::WorkBudgetExhausted { .. }
        | AgentError::NotAvailableWithoutTurn { .. }
        | AgentError::SerializationError(_)
        | AgentError::CompactNoLlm
        | AgentError::CompactSummaryBudgetInvalid { .. }
        | AgentError::CompactBudgetUnrecovered { .. }
        | AgentError::ChildTasksHandedOff { .. } => None,
        AgentError::LlmError(_) | AgentError::LlmHttpError { .. } | AgentError::ModelError(_) => {
            Some("model_error")
        }
        AgentError::StreamRecoveryExhausted { .. } => Some("stream_interrupted"),
        AgentError::CompactIncompleteResponse { .. }
        | AgentError::CompactRecoveryExhausted { .. }
        | AgentError::CompactRetriesExhausted { .. } => Some("compact_failed"),
        AgentError::ToolExecutionFailed { .. }
        | AgentError::ToolNotFound(_)
        | AgentError::ToolRejected { .. } => Some("tool_failed"),
        AgentError::OutputTruncated { .. } => Some("output_truncated"),
        AgentError::MaxIterationsExceeded(_) => Some("max_iterations"),
        AgentError::MiddlewareError { .. } => Some("middleware_error"),
        AgentError::Other(_) => Some("turn_error"),
    }
}

/// 一轮以 `LoopResult::Error` 结束后，为 active goal 安排恢复尝试。
///
/// 取消的 turn 不安排（用户取消优先）；没有 goal controller 或错误不可恢复时
/// 保持 no-op。
pub async fn schedule_goal_recovery_after_error(
    controller: Option<&Arc<dyn GoalController>>,
    error: &AgentError,
    cancelled: bool,
) {
    if cancelled {
        return;
    }
    let (Some(controller), Some(reason)) = (controller, goal_recovery_reason(error)) else {
        return;
    };
    controller.recover_after_error(reason).await;
}

#[cfg(test)]
#[path = "recovery_test.rs"]
mod tests;
