//! 会话加载预约：阻塞 prompt，并标记下一次切换是否仍会发生。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::super::interaction_lifecycle::SessionOperationId;

pub(super) struct SessionLoadReservationState {
    pub(super) reservations: Mutex<SessionLoadReservations>,
    pub(super) epoch_tx: watch::Sender<u64>,
}

impl SessionLoadReservationState {
    pub(super) fn new(epoch_tx: watch::Sender<u64>) -> Self {
        Self {
            reservations: Mutex::new(SessionLoadReservations::default()),
            epoch_tx,
        }
    }

    pub(super) fn pending(&self) -> usize {
        self.reservations.lock().unwrap().pending()
    }
}

/// 预约登记簿：总数用于阻塞 prompt，未认领队列用于确定用户最新的会话操作。
///
/// 预约在创建时取得操作身份并入队，在它所保护的 `session/load` 开始切换前被
/// 认领。身份而不是「是否存在未认领预约」决定车道：连续预约 B、C 时，最新
/// 未认领的 C 就是队列等待的目标，B 先开始也不会把输入改道。
#[derive(Default)]
pub(super) struct SessionLoadReservations {
    pending: usize,
    unclaimed: VecDeque<SessionOperationId>,
}

impl SessionLoadReservations {
    pub(super) fn reserve(&mut self, operation: SessionOperationId) {
        self.pending += 1;
        self.unclaimed.push_back(operation);
    }

    pub(super) fn claim(&mut self, operation: SessionOperationId) {
        self.unclaimed.retain(|candidate| *candidate != operation);
    }

    /// 认领最早的一个未认领预约：一次 load 只消费一次预约。
    pub(super) fn claim_oldest(&mut self) -> Option<SessionOperationId> {
        self.unclaimed.pop_front()
    }

    fn release(&mut self, operation: SessionOperationId) {
        self.pending = self.pending.saturating_sub(1);
        self.unclaimed.retain(|candidate| *candidate != operation);
    }

    pub(super) fn pending(&self) -> usize {
        self.pending
    }

    /// 用户最新的会话操作：最新一个尚未被 load 认领的预约。
    pub(super) fn newest_unclaimed(&self) -> Option<SessionOperationId> {
        self.unclaimed.back().copied()
    }
}

/// Synchronous queue-side ownership for an ordinary session load.
///
/// The reservation is acquired before a load request enters its async consumer.
/// Dropping the guard releases exactly one queued/in-flight load and wakes prompt
/// waiters. It is deliberately not Clone: the pending count represents queue
/// ownership, not arbitrary client clones.
pub(crate) struct SessionLoadReservation {
    state: Arc<SessionLoadReservationState>,
    operation: SessionOperationId,
}

impl SessionLoadReservation {
    pub(super) fn new(
        state: Arc<SessionLoadReservationState>,
        operation: SessionOperationId,
    ) -> Self {
        Self { state, operation }
    }

    /// 本次预约的操作身份；`load` 切换与队列车道都使用它。
    pub(super) fn operation(&self) -> SessionOperationId {
        self.operation
    }
}

impl Drop for SessionLoadReservation {
    fn drop(&mut self) {
        let mut reservations = self.state.reservations.lock().unwrap();
        reservations.release(self.operation);
        drop(reservations);
        self.state
            .epoch_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}
