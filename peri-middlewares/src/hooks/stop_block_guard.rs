//! Stop hook Block 连续次数状态机。
//!
//! 维护 `stop_block_count`（最多 8 次），用于避免 Stop hook 死循环：当
//! agent 反复被 Stop hook `Block` 时，连续 8 次后忽略 block 让 agent 正常结束。
//!
//! Guard 不持有 `state`，也不直接写入 state。Block 时返回 [`GuardDecision`]，
//! 由 middleware trait 方法负责按 decision 注入对应消息（见不变量注释）。

use parking_lot::Mutex;

/// 8 次连续 Block 上限。
pub const STOP_BLOCK_LIMIT: u32 = 8;

/// Stop Block 的处理结果。
pub enum GuardDecision {
    /// 不 Block，重置计数器，agent 正常结束。
    Pass,
    /// Block，注入 system-reminder 后继续；`count` 为当前连续次数（1..=8），
    /// `reason` 为 Stop hook 给出的 Block 原因。
    Block { count: u32, reason: String },
    /// 已超过 8 次上限，强制结束（计数器重置）。
    ForceFinish,
}

/// Stop Block 计数器状态机。
///
/// 由 `HookMiddleware` 通过 `Arc` 共享。
pub struct StopBlockGuard {
    count: Mutex<u32>,
}

impl StopBlockGuard {
    pub fn new() -> Self {
        Self {
            count: Mutex::new(0),
        }
    }

    /// 收到 `HookAction::Block` 时调用，返回后续处理决策。
    pub fn on_block(&self, reason: &str) -> GuardDecision {
        let mut count = self.count.lock();
        *count += 1;
        if *count > STOP_BLOCK_LIMIT {
            tracing::warn!(
                count = *count,
                "Stop hook block 连续超过 8 次，忽略 block 正常结束"
            );
            // Reset counter and let agent finish normally
            *count = 0;
            return GuardDecision::ForceFinish;
        }
        tracing::info!(
            count = *count,
            reason = %reason,
            "Stop hook blocked: injecting reason as system-reminder (Human) and continuing"
        );
        GuardDecision::Block {
            count: *count,
            reason: reason.to_string(),
        }
    }

    /// 收到非 Block action 时调用，重置计数器。
    pub fn on_non_block(&self) {
        *self.count.lock() = 0;
    }

    /// 当前连续 Block 次数（仅用于测试与诊断）。
    pub fn current_count(&self) -> u32 {
        *self.count.lock()
    }
}

impl Default for StopBlockGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// 构造 Stop hook block 的反馈正文。
///
/// canonical envelope 由 middleware producer 构造；本状态机仅负责正文，不接触 state。
pub fn format_stop_block_feedback_no_wrapper(reason: &str, count: u32) -> String {
    format!(
        "<stop_hook_feedback>\nThe Stop hook blocked because: {}\nPlease address this feedback and continue your work.\n(Block {}/8)\n</stop_hook_feedback>",
        reason, count
    )
}
