//! S5.1–S5.3 TUI 历史窗口模型：持久身份、驻留账本、页接纳与锚点决策。
//!
//! ## 边界
//!
//! 本模块只维护**窗口模型与账目**，不持有 `TuiRenderUnit` 正文，也不删除渲染对象。
//! `BridgeState.committed` 仍是展示正文的唯一强持有者；淘汰只生成
//! [`ReleasePlan`] 并从窗口索引/账本移除对应段，真正的正文与派生缓存释放在 S6.2
//! 的统一释放事务中完成（见 `ReleasePlan` 与 `HistoryWindow::take_pending_release_plans`）。
//!
//! 来源身份对齐 S7.4：`frozenSnapshotId + ancestorThreadId`。
//!
//! ## 开关
//!
//! [`HistoryWindowConfig::history_window_v1`] 默认 `false`。关闭时所有接纳/淘汰入口
//! 返回 `Disabled` 且不改变账本；旧分页与渲染路径逐字节保持原样。配置对象由调用方
//! 显式构造（S5 不修改 `peri-config`），后续切片接入设置项时只需替换构造来源。

mod admission;
mod anchor;
mod budget;
mod commit;
mod detail;
mod diagnostics;
mod display;
mod index;
mod model;
mod release;
mod source;
mod viewport;

pub use admission::{
    AdmissionDecision, AdmissionRejection, AdmissionRejectionReason, EvictionBlocked, EvictionPlan,
    PageDraft, PagePlacement, PinnedReport, ReleaseSummary, ReserveOutcome, ReserveRejection,
};
pub use anchor::{
    AnchorRelocation, GapSeek, GapSeekDirection, merge_adjacent_gaps, select_gap_seek,
};
pub use budget::{WindowBudget, WindowLimits};
pub use commit::{CommitOutcome, HistoryCommitInfo};
pub use detail::{
    DetailDescriptor, HistoryDetailTarget, clear_detail_targets, placeholder_hash,
    publish_detail_targets, remove_detail_targets,
};
pub use diagnostics::{HistoryWindowDiagnostics, WindowStats};
pub use display::DisplayLedger;
pub use index::{
    AllocationAccount, EntryLocator, ResidentIndex, ResultRef, ToolCallKey, ToolCallState,
    ToolPairOutcome, ToolPairRejected, ToolTerminalMeta,
};
pub use model::{
    BoundedSet, DurableWatermark, GapId, GapPlaceholder, HistoryEntryView, HistoryGap,
    HistoryWindow, PageReservation, PinReason, PinReasons, RegionKey, ReleasePlan, ReleaseReason,
    ResidentSegment, SegmentDraft, SegmentKey, SegmentOrigin, SegmentPlacement, SelectionAnchor,
    WindowAnchor, WindowAnchorOffset, WindowIdentity, WindowModelError, WindowStatus,
};
pub use release::{ReleaseOutcome, release_window_plans};
pub use source::{HistorySource, PersistentEntryKey, PersistentSubItemKey};
pub use viewport::{
    WindowUnitMapping, WindowUnitToken, WindowViewport, clear_window_unit_mappings,
    clear_window_viewport, publish_viewport_from_token, publish_viewport_unanchored,
    publish_window_unit_mappings, publish_window_viewport, remove_window_unit_mappings,
    update_window_viewport_follow,
};

use self::budget::INITIAL_CHECKPOINT_HISTORY;

/// 窗口配置。默认关闭，初始预算见 [`WindowLimits`]；这些数字是实施初值，不是性能结论。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryWindowConfig {
    pub history_window_v1: bool,
    pub limits: WindowLimits,
}

impl HistoryWindowConfig {
    /// 显式启用（测试与后续设置接入使用）；预算仍取初始值。
    pub fn enabled() -> Self {
        Self {
            history_window_v1: true,
            ..Self::default()
        }
    }

    /// 检查点去重集合容量。
    pub fn checkpoint_history(&self) -> usize {
        INITIAL_CHECKPOINT_HISTORY
    }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod admission_test;
#[cfg(test)]
#[path = "anchor_test.rs"]
mod anchor_test;
#[cfg(test)]
#[path = "commit_test.rs"]
mod commit_test;
#[cfg(test)]
#[path = "index_test.rs"]
mod index_test;
/// 从配置文档读取 TUI 本地 beta（kebab-case `history-window-v1`，默认 false）。
pub fn history_window_beta_enabled(config: &crate::config::PeriConfig) -> bool {
    config
        .config
        .betas
        .get(peri_config::betas::HISTORY_WINDOW_V1)
        .unwrap_or(false)
}

/// 启动时把 beta 配置写入运行开关（只写一次；无热重载）。
pub fn apply_history_window_beta(enabled: bool) {
    crate::kit::atoms::HISTORY_WINDOW_V1.set(enabled);
}

#[cfg(test)]
#[path = "model_test.rs"]
mod model_test;
#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;
