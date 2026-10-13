//! Batcher 交付结果值类型（契约 §3.6）。
//!
//! 这些值类型只描述「已交付 / 待交付 / 待清理」的计数与终态，不携带 payload、地址或凭据；
//! 它们不是 HTTP 成功的同义词，也不允许被 `is_ok()` 当成「已完成」。

/// 一次交付屏障观察到的计数快照；单位一律是**事件**，不是文件也不是批次。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryCounts {
    /// 仍待交付的事件数；`pending_events >= persisted_pending_events`。
    pub pending_events: u64,
    /// 已持久接受、可由恢复路径在本轮之后继续交付的事件数（pending 的子集）。
    pub persisted_pending_events: u64,
    /// 无法确认整体交付的事件数（含服务端部分拒收所在批次，不重发整批）。
    pub failed_events: u64,
    /// 已登记等待清理的对象数；删除失败会保留该计数与对应配额。
    pub cleanup_pending_objects: u64,
    /// 仍持有任务或清理 owner 的活跃任务数。
    pub active_tasks: u64,
}

/// `flush` 的成功报告：捕获前缀（event_seq 与 prepare_seq 双水位）的事件全部到达终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushReport {
    Complete,
}

/// `shutdown` 的成功报告：全部事件已交付并完成清理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReport {
    Complete,
}
