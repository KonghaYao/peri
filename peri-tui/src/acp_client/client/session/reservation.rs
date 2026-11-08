//! Load reservations, prompt admission and transition claiming.

use std::sync::Arc;

use peri_acp::transport::types::AcpError;
use tokio::sync::watch;

use crate::acp_client::client::{AcpTuiClient, SessionLoadReservation};
use crate::acp_client::interaction_lifecycle::{
    PromptLease, TransitionKind, TransitionOperation, TransitionStart,
};

impl AcpTuiClient {
    /// Reserve an ordinary session load before handing it to an async consumer.
    /// This synchronous boundary closes the browser-select → consumer scheduling
    /// window where a fast submit could otherwise bind to the old Stable session.
    ///
    /// 操作身份在预约锁内分配：准入快照按同一把锁读取，因此不会看到「身份已
    /// 存在但预约尚未登记」的中间态。
    pub(crate) fn reserve_session_load(&self) -> SessionLoadReservation {
        let mut reservations = self.session_load_reservations.reservations.lock().unwrap();
        let operation = self.lifecycle.allocate_operation();
        reservations.reserve(operation);
        // 预约本身就是一次用户会话决定：与计数同锁递增，排队输入的准入快照
        // 因此不会读到「已预约、决策仍是旧的」的撕裂组合。
        self.bump_session_switch_seq();
        drop(reservations);
        self.session_load_reservations
            .epoch_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        SessionLoadReservation::new(Arc::clone(&self.session_load_reservations), operation)
    }

    #[cfg(test)]
    pub(crate) fn pending_session_load_count(&self) -> usize {
        self.session_load_reservations
            .reservations
            .lock()
            .unwrap()
            .pending()
    }

    pub(in crate::acp_client::client) async fn wait_for_session_load(
        &self,
        epoch_rx: &mut watch::Receiver<u64>,
    ) -> Result<(), AcpError> {
        loop {
            let _ = *epoch_rx.borrow_and_update();
            if self
                .session_load_reservations
                .reservations
                .lock()
                .unwrap()
                .pending()
                == 0
            {
                return Ok(());
            }
            epoch_rx.changed().await.map_err(|_| {
                AcpError::new(-32603, "session load reservation closed unexpectedly")
            })?;
        }
    }

    pub(in crate::acp_client::client) async fn open_prompt_after_session_loads(
        &self,
        request_id: Option<String>,
        expected: Option<&(String, u64)>,
    ) -> Result<(String, PromptLease), AcpError> {
        let mut epoch_rx = self.session_load_reservations.epoch_tx.subscribe();
        loop {
            let opened = {
                let _operation = self.lifecycle.operation_gate().lock().await;
                let reservations = self.session_load_reservations.reservations.lock().unwrap();
                let _ = *epoch_rx.borrow_and_update();
                if reservations.pending() > 0 {
                    None
                } else {
                    self.check_restore_error()?;
                    let identity = self
                        .lifecycle
                        .stable_identity()
                        .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
                    // 准入与 Stable 选择同锁完成：/clear、load 或启动恢复一旦切换
                    // 身份，入队时绑定的提交在这里被确定性拒绝，不改投新会话。
                    if let Some(expected) = expected
                        && &identity != expected
                    {
                        return Err(AcpError::new(
                            crate::acp_client::SESSION_BINDING_CHANGED_CODE,
                            "session changed before prompt",
                        ));
                    }
                    let session_id = identity.0;
                    let lease = self
                        .lifecycle
                        .open_prompt(request_id.clone())
                        .map_err(|message| AcpError::new(-32603, message))?;
                    // Keep the reservation mutex through open_prompt: a dispatcher
                    // linearizes strictly before this prompt or after it.
                    Some((session_id, lease))
                }
            };
            if let Some(opened) = opened {
                return Ok(opened);
            }
            self.wait_for_session_load(&mut epoch_rx).await?;
        }
    }

    /// 认领预约与路由切换在同一临界区完成。
    ///
    /// 准入快照按 `reservations → lifecycle` 的顺序加锁；这里保持同一顺序并
    /// 全程持有预约锁，因此快照只会看到「未认领 + 旧路由」或「已认领 +
    /// Transitioning」。分步执行会留下「已认领 + 旧 Stable」窗口，把用户选择
    /// B 之后的新输入绑定到 A 并随后错误取消。
    pub(super) fn claim_and_begin_transition(
        &self,
        reservation: Option<&SessionLoadReservation>,
        kind: TransitionKind,
        target: Option<String>,
        operation: TransitionOperation,
    ) -> Result<TransitionStart, &'static str> {
        let mut reservations = self.session_load_reservations.reservations.lock().unwrap();
        let operation = match reservation {
            Some(reservation) => {
                let operation = reservation.operation();
                reservations.claim(operation);
                TransitionOperation::Owned(operation)
            }
            // 直接调用方没有预约对象时消费最早一个：它保护的正是这次 load。
            None => match operation {
                TransitionOperation::New => match reservations.claim_oldest() {
                    Some(operation) => TransitionOperation::Owned(operation),
                    None => TransitionOperation::New,
                },
                other => other,
            },
        };
        #[cfg(test)]
        self.pause_before_session_route_change();
        self.lifecycle
            .begin_transition_with_operation(kind, target, operation)
    }
}
