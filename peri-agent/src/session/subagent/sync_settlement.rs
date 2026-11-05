//! sync child 交接后的终态与结果投影。
//!
//! 状态映射、父结果正文与 finalize 重试放在这里，避免 `run_sync.rs` 继续增长；
//! 结算的存储语义仍由 [`TerminalMqRoute`] 与资源 adapter 承担。

use std::time::Duration;

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::session::MqInput;
use peri_acp_types::thread::AgentStatus;

use super::terminal_settlement::TerminalMqRoute;
use super::types::{ChildRunOutcome, SubagentFailure};
use super::util::{extract_last_ai_text, format_handoff_notice};
use crate::session::Session;

pub(super) type SyncExecutionResult =
    Result<ChildRunOutcome, Box<dyn std::error::Error + Send + Sync>>;

/// finalize 的有界重试次数：稳定 ID 不变，重复提交只会得到既有结果。
const SETTLE_ATTEMPTS: usize = 3;

/// 终态映射（计划 A3）：只有真实取消映射 Cancelled + Stopped；
/// 其余中断、失败与未完成交接都按 Error + Idle 收口，不伪报 Done。
pub(super) fn terminal_status(outcome: &SyncExecutionResult, session: &Session) -> AgentStatus {
    match outcome {
        Ok(outcome) => terminal_status_for_outcome(outcome, session),
        Err(_) => failure_status(session),
    }
}

/// 执行体自身收尾（`ChildRunOutcome` 已解包）的同一映射。
pub(super) fn terminal_status_for_outcome(
    outcome: &ChildRunOutcome,
    session: &Session,
) -> AgentStatus {
    // 显式停止优先：取消后即使模型恰好收尾，也不能伪报 Done。
    if session.config().cancel_token.is_cancelled() {
        return AgentStatus::Cancelled;
    }
    match outcome {
        ChildRunOutcome::Settled => AgentStatus::Done,
        _ => AgentStatus::Error,
    }
}

fn failure_status(session: &Session) -> AgentStatus {
    if session.config().cancel_token.is_cancelled() {
        AgentStatus::Cancelled
    } else {
        AgentStatus::Error
    }
}

/// finalize 失败时用同一身份重试；不得改写 payload，否则同 ID 不同正文会
/// 变成 `Conflict`，registry 的交付会永久 pending。
pub(super) async fn settle_with_retry(
    route: &TerminalMqRoute,
    child: &str,
    status: AgentStatus,
    message: &MqInput,
) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=SETTLE_ATTEMPTS {
        match route.settle_message(child, status, message).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::warn!(
                    %child,
                    attempt,
                    %error,
                    "sync child terminal settlement failed; retrying with the same identity"
                );
                last_error = error;
                if attempt < SETTLE_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(25 * attempt as u64)).await;
                }
            }
        }
    }
    Err(last_error)
}

pub(super) fn parent_result(
    outcome: &SyncExecutionResult,
    session: &std::sync::Arc<Session>,
    task_id: &str,
    agent_name: &str,
    child_thread_id: &str,
) -> BackgroundTaskResult {
    let produced = extract_last_ai_text(session);
    let (success, output, subagent_failure) = match outcome {
        Ok(ChildRunOutcome::Settled) => (true, produced, None),
        Ok(ChildRunOutcome::Interrupted) => (
            false,
            "Sub-agent was interrupted after handoff".to_string(),
            None,
        ),
        Ok(ChildRunOutcome::HandedOff { receipt }) => {
            let failure =
                SubagentFailure::handed_off(&receipt.child_thread_id, agent_name, receipt.clone());
            (
                false,
                format!("{produced}\n\n{}", format_handoff_notice(receipt)),
                failure.safe_failure(),
            )
        }
        Ok(ChildRunOutcome::Failed { error, residual }) => {
            let failure = SubagentFailure::safe_failure_from_error(child_thread_id, error);
            (
                false,
                format!(
                    "Sub-agent failed after handoff: {}{}",
                    error.user_facing_message(),
                    residual
                        .as_ref()
                        .map(|receipt| format!("\n\n{}", format_handoff_notice(receipt)))
                        .unwrap_or_default()
                ),
                failure,
            )
        }
        Ok(ChildRunOutcome::HandoffFailed { cause, failure }) => {
            let mut output = String::new();
            if let Some(cause) = cause {
                output.push_str(&cause.user_facing_message());
                output.push_str("\n\n");
            }
            output.push_str(&format!(
                "{}: {}",
                super::types::handoff_failure_label(failure),
                failure.error
            ));
            if let Some(flush) = &failure.flush_error {
                output.push_str(&format!("\nhandoff history flush failed: {flush}"));
            }
            if let Some(receipt) = &failure.receipt {
                output.push_str("\n\n");
                output.push_str(&format_handoff_notice(receipt));
            }
            let safe = match cause {
                Some(cause) => SubagentFailure::safe_failure_from_error(child_thread_id, cause),
                None => {
                    let error = super::types::handoff_failure_agent_error(failure);
                    SubagentFailure::safe_failure_from_error(child_thread_id, &error)
                }
            };
            (false, output, safe)
        }
        Err(error) => {
            let failure = error
                .downcast_ref::<SubagentFailure>()
                .and_then(SubagentFailure::safe_failure);
            let residual = error
                .downcast_ref::<SubagentFailure>()
                .and_then(|failure| failure.handoff())
                .map(format_handoff_notice)
                .unwrap_or_default();
            (
                false,
                format!("Sub-agent failed after handoff: {error}{residual}"),
                failure,
            )
        }
    };
    BackgroundTaskResult {
        task_id: task_id.to_string(),
        agent_name: agent_name.to_string(),
        prompt_summary: "synchronous sub-agent handoff".into(),
        success,
        output,
        tool_calls_count: super::util::count_tool_calls_from_session(session),
        duration_ms: 0,
        timed_out: false,
        child_thread_id: Some(child_thread_id.to_string()),
        subagent_failure,
        shell_output: None,
    }
}
