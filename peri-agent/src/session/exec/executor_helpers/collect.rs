use std::sync::Arc;

use peri_acp_types::{event::ExecutorEvent, session::PromptResult};
use tokio::sync::oneshot;
use tracing::{debug, error};

use super::{event_pump::PumpHandle, ExecOutcome};

// ── Collect Result Request parameter object ─────────────────────────────────

/// 结果收集请求（参数对象）。
pub struct CollectRequest<'a> {
    pub event_tx:
        &'a Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    pub pump_handle: PumpHandle,
    pub session_id: &'a str,
    pub exec_outcome: ExecOutcome,
}

/// 最终结果收集：close channel → 等待 pump drain → 提取 recall items。
///
/// 顺序约束：必须先 close event_tx，pump 才能退出 recv 循环；然后等待 pump_done。
pub async fn collect_result(req: CollectRequest<'_>) -> PromptResult {
    let CollectRequest {
        event_tx,
        pump_handle,
        session_id,
        mut exec_outcome,
    } = req;

    close_channel(event_tx);
    wait_for_pump(pump_handle.pump_done_rx, session_id).await;

    let recall_items = exec_outcome.agent_state.drain_recall();
    PromptResult {
        messages: exec_outcome.agent_state.into_messages(),
        ok: exec_outcome.ok,
        stop_reason: exec_outcome.stop_reason,
        history_replaced_by_compaction: exec_outcome.history_replaced_by_compaction,
        recall_items,
    }
}

pub fn close_channel(
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
) {
    let mut tx_guard = event_tx.lock();
    *tx_guard = None;
}

pub async fn wait_for_pump(pump_done_rx: oneshot::Receiver<()>, session_id: &str) {
    match tokio::time::timeout(std::time::Duration::from_secs(10), pump_done_rx).await {
        Ok(Ok(())) => debug!(session_id, "Event pump done"),
        Ok(Err(_)) => error!(session_id, "Event pump done channel closed unexpectedly"),
        Err(_) => error!(
            session_id,
            "Event pump timed out (10s) — Langfuse flush may have blocked push_done"
        ),
    }
}
