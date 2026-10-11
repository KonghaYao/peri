//! 单 run 的 limits 运行期状态：tool 调用计数与 breach 的共享真相。
//!
//! P6 前计数发生在 `agent/run` 分派路径（执行 task 持有 permit）；改为任务表驱动后，
//! 计数来源是任务终态结果（投递路径前置观察），读取方是消息循环
//! （`workflow/done` 的终态投影）。breach 只写首次值（沿用旧 `limit_breach` 语义）。

use std::sync::atomic::{AtomicU64, Ordering};

/// 单 run limit 状态（消息循环与任务投递路径共享；进程内、随 run 生命周期）。
pub(crate) struct RunLimitState {
    observed_tool_calls: AtomicU64,
    max_tool_calls: Option<u64>,
    breach: parking_lot::Mutex<Option<String>>,
}

impl RunLimitState {
    pub(crate) fn new(max_tool_calls: Option<u64>) -> Self {
        Self {
            observed_tool_calls: AtomicU64::new(0),
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

    /// 累计一次任务结果的 tool 调用计数；超上限时记录 breach 并返回详情。
    pub(crate) fn observe_tool_calls(&self, count: u64) -> Option<String> {
        let total = self
            .observed_tool_calls
            .fetch_add(count, Ordering::SeqCst)
            .saturating_add(count);
        let maximum = self.max_tool_calls?;
        if total > maximum {
            let detail = format!("workflow exceeded maxToolCalls ({maximum})");
            self.set_breach(detail.clone());
            return Some(detail);
        }
        None
    }
}
