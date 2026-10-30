use std::collections::{HashMap, VecDeque};

use peri_acp_types::tasks::TaskRecord;

// Snapshot is a current-task directory with a bounded recent terminal window,
// not a session's execution history. Business replay uses producer tickets.
pub(super) const TERMINAL_RECORD_LIMIT: usize = 512;
pub(super) const TERMINAL_BYTE_LIMIT: usize = 1024 * 1024;
pub(super) const SUMMARY_BYTE_LIMIT: usize = 1024;

#[derive(Default)]
pub(super) struct TaskProjection {
    pub(super) revision: u64,
    pub(super) records: HashMap<String, TaskRecord>,
    terminal_order: VecDeque<String>,
    terminal_bytes: usize,
}

fn retained_bytes(record: &TaskRecord) -> usize {
    std::mem::size_of::<TaskRecord>()
        // Record, HashMap key and terminal-order key each own an ID allocation.
        + std::mem::size_of::<String>() * 2
        + record.task_id.capacity() * 3
        + record.summary.capacity()
        + record.started_at.capacity()
        + record.status.capacity()
        + record.output_preview.as_ref().map_or(0, String::capacity)
        + record
            .initiator_session_id
            .as_ref()
            .map_or(0, String::capacity)
        + record.child_thread_id.as_ref().map_or(0, String::capacity)
}

impl TaskProjection {
    pub(super) fn insert(&mut self, record: TaskRecord) {
        if self.terminal_order.iter().any(|id| id == &record.task_id) {
            self.terminal_order.retain(|id| id != &record.task_id);
            if let Some(old) = self.records.get(&record.task_id) {
                self.terminal_bytes = self.terminal_bytes.saturating_sub(retained_bytes(old));
            }
        }
        self.records.insert(record.task_id.clone(), record);
    }

    // Called only once the registry has released every delivery/cleanup owner.
    pub(super) fn retire(&mut self, task_id: &str) -> bool {
        let Some(record) = self.records.get(task_id) else {
            return false;
        };
        let mut removed = false;
        self.terminal_bytes += retained_bytes(record);
        self.terminal_order.push_back(task_id.to_owned());
        while self.terminal_order.len() > TERMINAL_RECORD_LIMIT
            || self.terminal_bytes > TERMINAL_BYTE_LIMIT
        {
            let Some(oldest) = self.terminal_order.pop_front() else {
                break;
            };
            if let Some(record) = self.records.remove(&oldest) {
                removed = true;
                self.terminal_bytes = self.terminal_bytes.saturating_sub(retained_bytes(&record));
            }
        }
        let target = self.records.len().max(TERMINAL_RECORD_LIMIT);
        if self.records.capacity() > target.saturating_mul(2) {
            self.records.shrink_to(target);
        }
        removed
    }
}
