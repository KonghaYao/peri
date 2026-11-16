//! One classification of prompt completion, turn status, and execution diagnostics.

use crate::agent::stages::LoopResult;
use peri_acp_types::command::PromptStopReason;
use peri_acp_types::error::AgentError;
use peri_acp_types::event::TurnErrorKind;
use peri_acp_types::event::TurnStatus;
use peri_acp_types::session::ExecutionFailure;
use tracing::error;

/// Phase 9 的单一终态分类结果。
pub(crate) struct LoopTerminal {
    pub(crate) ok: bool,
    pub(crate) stop_reason: PromptStopReason,
    pub(crate) failure: Option<ExecutionFailure>,
    pub(crate) turn_status: TurnStatus,
    pub(crate) turn_error_kind: Option<TurnErrorKind>,
}

pub(super) fn internal_failure_terminal(failure: ExecutionFailure) -> LoopTerminal {
    LoopTerminal {
        ok: false,
        stop_reason: PromptStopReason::EndTurn,
        failure: Some(failure),
        turn_status: TurnStatus::Error,
        turn_error_kind: Some(TurnErrorKind::Internal),
    }
}

/// Phase 9 纯分类器：一次同时决定 Prompt、Turn 和 fatal failure。
///
/// 分类契约（spec/history/2026-08.md 2026-08-18 条目 Commit 1）：
/// - `Completed` / `HookStopped` / 用户 cancel / `Interrupted` /
///   `MaxIterationsExceeded` → failure 为 `None`（它们已有标准 `StopReason`
///   表达，不应升级为请求失败）；
/// - 其他 `LoopResult::Error` → failure 为安全的窄投影：LLM 错误保留脱敏、
///   限长后的原始含义和可选 HTTP status，其他错误使用安全用户文案。
///
/// `HookStopped` 只终止当前 run 的模型请求；主会话与 `Completed` 一样正常
/// 结束（子 Agent 的交接语义在 runner 消费该结果后已经完成）。
///
/// fatal 判定与 `AgentExecutionFailed` 发射条件共享（fatal ↔ 发射私有事件），
/// public message 由 [`ExecutionFailure::from_agent_error`] 统一生成。
pub(crate) fn classify_loop_terminal(
    loop_result: &LoopResult,
    sampled_cancel: bool,
) -> LoopTerminal {
    if matches!(
        loop_result,
        LoopResult::Completed | LoopResult::HookStopped { .. }
    ) {
        return LoopTerminal {
            ok: true,
            stop_reason: PromptStopReason::EndTurn,
            failure: None,
            turn_status: TurnStatus::Done,
            turn_error_kind: None,
        };
    }
    if sampled_cancel
        || matches!(
            loop_result,
            LoopResult::Interrupted | LoopResult::Error(AgentError::Interrupted)
        )
    {
        return LoopTerminal {
            ok: false,
            stop_reason: PromptStopReason::Cancelled,
            failure: None,
            turn_status: TurnStatus::Interrupted,
            turn_error_kind: Some(TurnErrorKind::Interrupted),
        };
    }
    if matches!(
        loop_result,
        LoopResult::Error(AgentError::MaxIterationsExceeded(_))
    ) {
        return LoopTerminal {
            ok: false,
            stop_reason: PromptStopReason::MaxTurnRequests,
            failure: None,
            turn_status: TurnStatus::Error,
            turn_error_kind: Some(TurnErrorKind::MaxIterations),
        };
    }
    if matches!(
        loop_result,
        LoopResult::Error(AgentError::OutputTruncated { .. })
    ) {
        return LoopTerminal {
            ok: false,
            stop_reason: PromptStopReason::MaxTokens,
            failure: None,
            turn_status: TurnStatus::Error,
            turn_error_kind: Some(TurnErrorKind::LlmFailure),
        };
    }
    // `SettlementDue` 只应由 child loop 产生并由 child runner 消费；主 executor
    // 若收到它，说明调用边界被混用。显式失败而不是 panic，也不伪装成正常结束。
    if matches!(loop_result, LoopResult::SettlementDue { .. }) {
        return internal_failure_terminal(ExecutionFailure::internal(
            "child settlement result reached the main executor",
        ));
    }
    let LoopResult::Error(error) = loop_result else {
        unreachable!("Completed, HookStopped, Interrupted and classified errors were handled above")
    };
    LoopTerminal {
        ok: false,
        stop_reason: PromptStopReason::EndTurn,
        failure: Some(ExecutionFailure::from_agent_error(error)),
        turn_status: TurnStatus::Error,
        turn_error_kind: Some(TurnErrorKind::LlmFailure),
    }
}

pub(super) fn log_loop_terminal(
    session_id: &str,
    turn_id: &str,
    loop_result: &LoopResult,
    terminal: &LoopTerminal,
    finalization_failed: bool,
) {
    if let Some(failure) = &terminal.failure {
        error!(
            session_id = %session_id,
            turn_id = %turn_id,
            diagnostic_origin = "executor_terminal",
            kind = failure.kind.wire_name(),
            category = if finalization_failed { Some("execution_finalize") } else { match loop_result { LoopResult::Error(error) => Some(error.category_name()), _ => None } },
            http_status = failure.http_status,
            message = %failure.public_message,
            loop_error_category = match loop_result { LoopResult::Error(error) => Some(error.category_name()), _ => None },
            loop_error = ?match loop_result { LoopResult::Error(error) => Some(error.user_facing_message()), _ => None },
            causes = ?match loop_result { LoopResult::Error(error) => error.cause_chain(), _ => Vec::new() },
            diagnostic = ?failure.diagnostic,
            "[v2] execution failed"
        );
    } else if let LoopResult::Error(error) = loop_result {
        tracing::debug!(
            session_id = %session_id,
            turn_id = %turn_id,
            stop_reason = ?terminal.stop_reason,
            category = error.category_name(),
            causes = ?error.cause_chain(),
            "[v2] non-fatal loop termination"
        );
    }
}
