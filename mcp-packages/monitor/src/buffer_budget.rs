//! Instance-wide accounting and eviction for active and terminal event rings.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use parking_lot::Mutex;

use crate::buffer::{CapturedEvent, EventBuffer};

/// Event storage shared by every task in one monitor instance.
pub(crate) const INSTANCE_EVENT_BYTES: usize = 32 * 1024 * 1024;
const RECLAIM_BATCH_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct BudgetState {
    buffers: HashMap<String, Arc<Mutex<EventBuffer>>>,
    victims: VecDeque<String>,
    resident_bytes: usize,
}

pub(crate) struct BufferBudget {
    max_bytes: usize,
    state: Mutex<BudgetState>,
}

impl BufferBudget {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            state: Mutex::new(BudgetState::default()),
        }
    }

    pub(crate) fn register(&self, id: String, buffer: Arc<Mutex<EventBuffer>>) {
        let mut state = self.state.lock();
        debug_assert!(!state.buffers.contains_key(&id));
        state.resident_bytes += buffer.lock().resident_bytes();
        state.buffers.insert(id.clone(), buffer);
        state.victims.push_back(id);
    }

    /// All writers enter this gate before locking a task buffer. Readers only
    /// lock the buffer; no reader can retain an unaccounted ring allocation.
    pub(crate) fn push(&self, id: &str, event: CapturedEvent) -> Option<u64> {
        let mut state = self.state.lock();
        let buffer = state.buffers.get(id)?.clone();
        let seq = {
            let mut buffer = buffer.lock();
            let before = buffer.resident_bytes();
            let seq = buffer.push(event);
            state.resident_bytes = state.resident_bytes - before + buffer.resident_bytes();
            seq
        };
        if state.resident_bytes > self.max_bytes {
            // Reclaim in batches so a full instance does not scan 128 task
            // buffers for every output line. Each ring keeps its own order.
            let target = self
                .max_bytes
                .saturating_sub(RECLAIM_BATCH_BYTES.min(self.max_bytes / 4));
            while state.resident_bytes > target {
                let Some(victim) = state.victims.pop_front() else {
                    break;
                };
                let buffer = state.buffers[&victim].clone();
                let mut buffer = buffer.lock();
                let before = buffer.resident_bytes();
                let reclaim = state.resident_bytes - target;
                buffer.reclaim_to(before.saturating_sub(reclaim));
                state.resident_bytes = state.resident_bytes - before + buffer.resident_bytes();
                state.victims.push_back(victim);
            }
        }
        Some(seq)
    }

    /// Empty and shrink while still under the accounting gate. A concurrent
    /// reader holding a task Arc therefore cannot keep its old payload alive.
    pub(crate) fn remove(&self, id: &str) {
        let mut state = self.state.lock();
        if let Some(buffer) = state.buffers.remove(id) {
            let mut buffer = buffer.lock();
            let before = buffer.resident_bytes();
            buffer.reclaim_to(0);
            state.resident_bytes -= before - buffer.resident_bytes();
            state.victims.retain(|candidate| candidate != id);
        }
    }

    #[cfg(test)]
    pub(crate) fn resident_bytes(&self) -> usize {
        self.state.lock().resident_bytes
    }
}

#[cfg(test)]
#[path = "buffer_budget_test.rs"]
mod tests;
