//! 关闭门：run 准入、任务发布与实例关闭共享的领域准入栅栏。
//!
//! 语义（design §5.5 关闭顺序第 ① 步「停止接受新 run 与新任务发布」）：
//! `mark_closed` 返回后，不再放行新的 run 准入，也不再放行新的任务发布；
//! 持票（临界区）内已完成的注册属于关闭前提交，由后续「取消 / 有界等待」收敛。
//! 回填（`settle`）与取消不经此门：关闭顺序第 ②③ 步需要它们保持可用。
//!
//! 实现：准入取读票据，关闭取写锁——关闭等待全部在途准入结束，准入在关闭后
//! 一律拒绝。只设一个布尔位（先查后注册）会在两者之间留下窗口（对抗审查 F6）。

use parking_lot::{RwLock, RwLockReadGuard};

/// 关闭门（`WorkflowRuntime` 持有，任务表共享同一实例）。
#[derive(Default)]
pub struct CloseGate {
    closed: RwLock<bool>,
}

impl CloseGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.read()
    }

    /// 标记关闭：等待在途准入（持票者）结束后置位；返回后不再放行新准入。
    pub fn mark_closed(&self) {
        *self.closed.write() = true;
    }

    /// 准入：返回 `None` 表示已关闭，调用方不得注册任何新 run / 新任务。
    ///
    /// 票据必须覆盖「校验 + 注册」整个临界区（准入与注册对外是一个原子边界）。
    pub fn admit(&self) -> Option<AdmissionTicket<'_>> {
        let guard = self.closed.read();
        (!*guard).then_some(AdmissionTicket { _guard: guard })
    }
}

/// 准入票据：持有期间 `mark_closed` 阻塞。
pub struct AdmissionTicket<'a> {
    _guard: RwLockReadGuard<'a, bool>,
}

#[cfg(test)]
#[path = "close_gate_test.rs"]
mod tests;
