//! 执行尝试记账：区分引擎重试与 journal 缓存命中。
//!
//! 引擎在 hooks 内部失败后会自动重试一次，只有最终结果写入 journal；仅看
//! journal 条目会漏掉首次尝试。宿主在每次 `agent/task` 准入时计数，并在
//! `journal/append` 时把序号补进 [`peri_acp_types::workflow::WorkflowAttempt`]。
//! 缓存命中没有本次 `agent/task`，因此保留旧序号或显式为 `None`。

use std::collections::HashMap;
use std::collections::HashSet;

use crate::protocol::JournalEntry;

#[derive(Debug, Default)]
pub(crate) struct AttemptTracker {
    dispatches: HashMap<(String, u64), u32>,
    seen_tasks: HashSet<(String, String)>,
}

impl AttemptTracker {
    /// 记录一次已准入的 agent 执行尝试；`agent_id` 同时是引擎 journal 的 seq。
    ///
    /// 任务通知可能重复投递；同一 task 只计一次，不同 task 的重试各计一次。
    pub(crate) fn record_dispatch(&mut self, run_id: &str, agent_id: u64, task_id: &str) {
        if !self
            .seen_tasks
            .insert((run_id.to_string(), task_id.to_string()))
        {
            return;
        }
        let count = self
            .dispatches
            .entry((run_id.to_string(), agent_id))
            .or_insert(0);
        *count = count.saturating_add(1);
    }

    /// 给 journal attempt 补本 run 的尝试序号；恢复条目保留原序号。
    pub(crate) fn annotate(&self, entry: &mut JournalEntry) {
        let Some(attempt) = entry.attempt.as_mut() else {
            return;
        };
        if attempt.attempt_index.is_some() {
            return;
        }
        let Some(index) = self
            .dispatches
            .get(&(attempt.run_id.clone(), entry.seq))
            .copied()
        else {
            return;
        };
        attempt.attempt_index = Some(index);
        if attempt.agent_id.is_none() {
            attempt.agent_id = Some(entry.seq);
        }
    }
}
