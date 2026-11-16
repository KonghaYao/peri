//! 单 run 的 limits 运行期状态：tool 调用计数与 breach 的共享真相。
//!
//! P6 前计数发生在 `agent/run` 分派路径（执行 task 持有 permit）；改为任务表驱动后，
//! 计数来源是任务终态结果（投递路径前置观察），读取方是消息循环
//! （`workflow/done` 的终态投影）。breach 只写首次值（沿用旧 `limit_breach` 语义）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

/// 单 run limit 状态（消息循环与任务投递路径共享；进程内、随 run 生命周期）。
pub(crate) struct RunLimitState {
    observed_tool_calls: AtomicU64,
    /// 已计入 tool 调用的任务身份（taskId 作用域 = 单次 run）：终态回投是投递重试，
    /// 不是第二次执行，计数只认首次。集合随 run 生命周期回收，规模与 run 内任务数同阶。
    counted_tasks: parking_lot::Mutex<HashSet<String>>,
    max_tool_calls: Option<u64>,
    breach: parking_lot::Mutex<Option<String>>,
}

impl RunLimitState {
    pub(crate) fn new(max_tool_calls: Option<u64>) -> Self {
        Self {
            observed_tool_calls: AtomicU64::new(0),
            counted_tasks: parking_lot::Mutex::new(HashSet::new()),
            max_tool_calls,
            breach: parking_lot::Mutex::new(None),
        }
    }

    /// 已记录的 breach 详情（`workflow/done` 终态投影读取）。
    pub(crate) fn breach(&self) -> Option<String> {
        self.breach.lock().clone()
    }

    /// 记录 breach（只写首次；后到的超限不覆写首个错误文本）。
    pub(crate) fn set_breach(&self, detail: String) {
        let mut breach = self.breach.lock();
        if breach.is_none() {
            *breach = Some(detail);
        }
    }

    /// 累计一次任务结果的 tool 调用计数；同一 task 的重复回填只计首次。
    ///
    /// 任务表允许对已终态任务重复 `settle` 以重试投递（design §5.1）：投递可重试，
    /// 计数不能重算，否则同一批 tool 调用被重复累计，未超限的 run 会误报 `maxToolCalls`。
    pub(crate) fn observe_tool_calls(&self, task_id: &str, count: u64) -> Option<String> {
        let Some(maximum) = self.max_tool_calls else {
            // 无上限时不产生 breach，也无需为去重保留任务身份。
            return None;
        };
        if !self.counted_tasks.lock().insert(task_id.to_owned()) {
            return None;
        }
        let total = self
            .observed_tool_calls
            .fetch_add(count, Ordering::SeqCst)
            .saturating_add(count);
        if total > maximum {
            let detail = format!("workflow exceeded maxToolCalls ({maximum})");
            self.set_breach(detail.clone());
            return Some(detail);
        }
        None
    }
}
