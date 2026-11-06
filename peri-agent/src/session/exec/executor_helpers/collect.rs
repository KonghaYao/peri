use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::{
    event::ExecutorEvent,
    session::{ExecutionFailure, PromptResult},
};
use tokio::sync::oneshot;
use tracing::{debug, error};

use super::{event_pump::PumpHandle, ExecOutcome};

const EVENT_PUMP_WAIT: Duration = Duration::from_secs(10);

// ── Collect Result Request parameter object ─────────────────────────────────

/// 结果收集请求（参数对象）。
pub struct CollectRequest<'a> {
    pub event_tx:
        &'a Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    pub pump_handle: PumpHandle,
    pub session_id: &'a str,
    pub exec_outcome: ExecOutcome,
    /// turn 退出时仍未结算的后台任务数（§7.3 摘要，由调用方在 loop 结束后读取）。
    pub pending_tasks: u32,
}

/// 最终结果收集：close channel → 等待 pump drain → 提取 recall items。
///
/// 顺序约束：必须先 close event_tx，pump 才能退出 recv 循环；然后等待 pump_done。
/// 泵未在窗口内送达终态时，本轮必须以失败结算——`done` 缺失会让客户端停在 loading。
pub async fn collect_result(req: CollectRequest<'_>) -> PromptResult {
    let CollectRequest {
        event_tx,
        pump_handle,
        session_id,
        mut exec_outcome,
        pending_tasks,
    } = req;

    close_channel(event_tx);
    let pump_failure = wait_for_pump(pump_handle.pump_done_rx, session_id).await;

    let recall_items = exec_outcome.agent_state.drain_recall();
    // 占用快照必须在 `into_messages` 之前取：tracker 随 agent_state 一起被消费。
    let context_usage_tokens = exec_outcome
        .agent_state
        .token_tracker
        .estimated_context_tokens();
    let mut result = PromptResult {
        persisted_payloads: exec_outcome.persisted_payloads,
        messages: exec_outcome.agent_state.into_messages(),
        ok: exec_outcome.ok,
        stop_reason: exec_outcome.stop_reason,
        failure: exec_outcome.failure,
        persistence_inconsistent: exec_outcome.persistence_inconsistent,
        history_replaced_by_compaction: exec_outcome.history_replaced_by_compaction,
        recall_items,
        pending_tasks,
        context_usage_tokens,
    };
    if let Err(failure) = pump_failure {
        // 执行结果已产生但终态事件未交付：不能按完成报告，否则客户端无法收敛
        // loading；已有执行失败保留原始归因。
        result.ok = false;
        result.failure.get_or_insert(failure);
    }
    result
}

pub fn close_channel(
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
) {
    let mut tx_guard = event_tx.lock();
    *tx_guard = None;
}

/// 等待事件泵结算。`Err` 表示终态 `done` 未被确认交付，调用方不得按成功返回。
pub async fn wait_for_pump(
    pump_done_rx: oneshot::Receiver<()>,
    session_id: &str,
) -> Result<(), ExecutionFailure> {
    match peri_time::timeout(EVENT_PUMP_WAIT, pump_done_rx).await {
        Ok(Ok(())) => {
            debug!(session_id, "Event pump done");
            Ok(())
        }
        Ok(Err(_)) => {
            error!(session_id, "Event pump ended without signaling completion");
            Err(ExecutionFailure::internal(
                "event pump ended without signaling completion; the terminal done notification may not have been delivered",
            ))
        }
        Err(_) => {
            error!(
                session_id,
                "Event pump timed out (10s) waiting for terminal delivery"
            );
            Err(ExecutionFailure::internal(
                "event pump did not finish within 10s; the terminal done notification may not have been delivered",
            ))
        }
    }
}
