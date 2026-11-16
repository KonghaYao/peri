//! History-window configuration and initial budgets.

use super::WindowLimits;
use super::budget::INITIAL_CHECKPOINT_HISTORY;

/// 窗口配置。默认关闭，初始预算见 [`WindowLimits`]；这些数字是实施初值，不是性能结论。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryWindowConfig {
    pub history_window_v1: bool,
    pub limits: WindowLimits,
}

impl HistoryWindowConfig {
    /// 显式启用（测试与后续设置接入使用）；预算仍取初始值。
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            history_window_v1: true,
            ..Self::default()
        }
    }

    /// 检查点去重集合容量。
    #[must_use]
    pub fn checkpoint_history(&self) -> usize {
        INITIAL_CHECKPOINT_HISTORY
    }
}
