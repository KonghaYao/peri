//! Exec outcome.

use super::{fmt, AgentState, Debug, ExecutionFailure, Formatter, PromptStopReason};

// 普通执行与 Immediate 命令共用 done 协议投影，避免响应已取消而通知仍正常完成。
pub(super) fn done_stop_reason(reason: PromptStopReason) -> &'static str {
    match reason {
        PromptStopReason::EndTurn => "end_turn",
        PromptStopReason::Cancelled => "cancelled",
        PromptStopReason::MaxTurnRequests => "max_turn_requests",
        PromptStopReason::MaxTokens => "max_tokens",
    }
}

// ── 共享类型（L5：自 ACP executor.rs 迁入）──────────────────────────────────

/// Agent 执行后的最终输出（state + 停止原因）。
#[must_use]
pub struct ExecOutcome {
    pub execution_binding: Option<peri_acp_types::session::ExecutionBinding>,
    pub ok: bool,
    pub stop_reason: PromptStopReason,
    /// 致命执行失败（None = 正常终止 / 用户取消 / 最大轮数；Some = 真正
    /// fatal 的 `LoopResult::Error`，见
    /// spec/history/2026-08.md 2026-08-18 条目 Commit 1）。
    pub failure: Option<ExecutionFailure>,
    /// A Full Compact committed during this turn and replaced prior visible history.
    pub history_replaced_by_compaction: bool,
    /// Canonical transcript snapshot captured after the persistence barrier.
    /// 只含**有效**（未排除）条目：Full Compact 提交释放的 own 原文不在其中，
    /// 完整历史（含归档原文）只能从存储读取。
    pub persisted_payloads: Vec<peri_acp_types::store::PersistedPayload>,
    /// Durable state could not be rolled back or verified; host must invalidate the session.
    pub persistence_inconsistent: bool,
    pub agent_state: AgentState,
}

/// turn 退出时仍未结算的后台任务数（§7.3 有界等待摘要）。
///
/// 与 `idle_should_wait` 的 busy 判据同源（`active_count`）：任务终态结算后
/// 即不再计入；`0` 表示无未结算任务，ACP 响应不附加 pending 标记。
pub(crate) fn pending_task_count(
    manager: Option<&std::sync::Arc<dyn peri_acp_types::tasks::TaskManager>>,
) -> u32 {
    manager
        .map(|manager| manager.active_count_including_children() as u32)
        .unwrap_or(0)
}

impl Debug for ExecOutcome {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecOutcome")
            .field(
                "execution_binding_present",
                &self.execution_binding.is_some(),
            )
            .field("ok", &self.ok)
            .field("failure_present", &self.failure.is_some())
            .field(
                "history_replaced_by_compaction",
                &self.history_replaced_by_compaction,
            )
            .field("persisted_payloads_len", &self.persisted_payloads.len())
            .field("persistence_inconsistent", &self.persistence_inconsistent)
            .finish_non_exhaustive()
    }
}
