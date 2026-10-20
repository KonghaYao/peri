//! 后台任务状态的单一权威映射。
//!
//! 文本标签（Tasks 面板）与取消判据共用这里的分类：同一条状态字符串不能在两处
//! 各自解释，否则「面板显示已完成、Enter 仍发取消」之类的分叉会重新出现。

use crate::i18n;

/// 后台任务状态类别——wire 上的状态字符串在此收敛。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BgTaskStatus {
    Running,
    Lost,
    Reconciling,
    PendingDelivery,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

impl BgTaskStatus {
    /// 分类 `BgTaskEntry.status`。
    ///
    /// `None` 表示 started 事件尚未携带状态（当前实现即如此），沿用既有口径视为运行中；
    /// 未知值不猜测语义，单独归类为 [`Self::Unknown`]。
    pub(crate) fn classify(status: Option<&str>) -> Self {
        match status {
            None | Some("running") => Self::Running,
            Some("lost") => Self::Lost,
            Some("reconciling") => Self::Reconciling,
            Some("pending_delivery" | "delivery_pending") => Self::PendingDelivery,
            Some("completed") => Self::Completed,
            Some("failed") => Self::Failed,
            Some("cancelled") => Self::Cancelled,
            Some(_) => Self::Unknown,
        }
    }

    /// i18n key——文本标签的唯一来源。
    pub(crate) fn i18n_key(self) -> &'static str {
        match self {
            Self::Running => "bg-task-status-running",
            Self::Lost => "bg-task-status-lost",
            Self::Reconciling => "bg-task-status-reconciling",
            Self::PendingDelivery => "bg-task-status-pending-delivery",
            Self::Completed => "bg-task-status-completed",
            Self::Failed => "bg-task-status-failed",
            Self::Cancelled => "bg-task-status-cancelled",
            Self::Unknown => "bg-task-status-unknown",
        }
    }

    /// 用户可见文本标签。
    pub(crate) fn label(self) -> String {
        i18n::tr(self.i18n_key())
    }

    /// 终态：不能再请求取消。
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// 当前执行仍可观察为活动：只有 running（含缺省状态）可以占据活动行。
    pub(crate) fn is_active(self) -> bool {
        matches!(self, Self::Running)
    }
}
