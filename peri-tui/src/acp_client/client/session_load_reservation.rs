//! 会话加载预约：阻塞 prompt，并标记下一次切换是否仍会发生。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

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

/// 预约登记簿：总数用于阻塞 prompt，未认领队列用于判断下一次切换是否仍会发生。
///
/// 预约在创建时入队，在它所保护的 `session/load` 开始切换前被认领。
/// 「load 已 commit Stable、预约尚未释放」的窗口里没有未认领预约，准入因此
/// 不会把后续输入误判成下一次 generation。
#[derive(Default)]
pub(super) struct SessionLoadReservations {
    pending: usize,
    unclaimed: VecDeque<u64>,
    next_id: u64,
}

impl SessionLoadReservations {
    pub(super) fn reserve(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("session load reservation id overflow");
        self.pending += 1;
        self.unclaimed.push_back(id);
        id
    }

    pub(super) fn claim(&mut self, id: u64) {
        self.unclaimed.retain(|candidate| *candidate != id);
    }

    /// 认领最早的一个未认领预约：一次 load 只消费一次预约。
    pub(super) fn claim_oldest(&mut self) {
        self.unclaimed.pop_front();
    }

    fn release(&mut self, id: u64) {
        self.pending = self.pending.saturating_sub(1);
        self.unclaimed.retain(|candidate| *candidate != id);
    }

    pub(super) fn pending(&self) -> usize {
        self.pending
    }

    pub(super) fn has_unclaimed(&self) -> bool {
        !self.unclaimed.is_empty()
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
    id: u64,
}

impl SessionLoadReservation {
    pub(super) fn new(state: Arc<SessionLoadReservationState>, id: u64) -> Self {
        Self { state, id }
    }

    /// 认领本次预约：它保护的 load 即将开始自己的切换。
    pub(super) fn claim(&self) {
        let mut reservations = self.state.reservations.lock().unwrap();
        reservations.claim(self.id);
    }
}

impl Drop for SessionLoadReservation {
    fn drop(&mut self) {
        let mut reservations = self.state.reservations.lock().unwrap();
        reservations.release(self.id);
        drop(reservations);
        self.state
            .epoch_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}
