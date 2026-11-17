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
//! 注意这里说的是模型对象自身的默认构造；TUI 启动时的本地投影开关默认开启
//! （见 [`history_window_beta_enabled`]），两者层级不同。

mod admission;
mod anchor;
mod budget;
mod commit;
mod detail;
mod diagnostics;
mod display;
mod edge_cursor;
mod index;
mod model;
mod release;
mod source;
mod switch;
mod viewport;

pub use admission::{
    AdmissionDecision, AdmissionRejection, AdmissionRejectionReason, EvictionBlocked, EvictionPlan,
    PageDraft, PagePlacement, PinnedReport, ReleaseSummary, ReserveOutcome, ReserveRejection,
};
pub use anchor::{
    AnchorRelocation, GapSeek, GapSeekDirection, merge_adjacent_gaps, select_gap_seek,
};
pub(crate) use budget::{PENDING_TOOL_ENDS_BYTES_MAX, PENDING_TOOL_ENDS_LIMIT};
pub use budget::{WindowBudget, WindowLimits};
pub use commit::{CommitOutcome, HistoryCommitInfo};
pub use detail::{
    DetailDescriptor, HistoryDetailTarget, HistoryV2BodyBinding, clear_detail_targets,
    placeholder_hash, publish_detail_targets, remove_detail_targets,
};
pub use diagnostics::{HistoryWindowDiagnostics, WindowStats};
pub use display::DisplayLedger;
pub use index::{AllocationAccount, EntryLocator, ResidentIndex};
pub use model::{
    BoundedSet, DurableWatermark, GapId, GapPlaceholder, HistoryEntryView, HistoryGap,
    HistoryWindow, PageReservation, PinReason, PinReasons, RegionKey, ReleasePlan, ReleaseReason,
    ResidentSegment, SegmentDraft, SegmentKey, SegmentOrigin, SegmentPlacement, SelectionAnchor,
    WindowAnchor, WindowAnchorOffset, WindowIdentity, WindowModelError, WindowStatus,
};
pub use release::{ReleaseOutcome, release_window_plans};
pub use source::{HistorySource, PersistentEntryKey, PersistentSubItemKey};
pub use switch::WindowSwitchSnapshot;
pub use viewport::{
    WindowUnitMapping, WindowUnitToken, WindowViewport, clear_window_unit_mappings,
    clear_window_viewport, publish_viewport_from_token, publish_viewport_unanchored,
    publish_window_unit_mappings, publish_window_viewport, remove_window_unit_mappings,
    update_window_viewport_follow,
};

mod history_window_config;
pub use history_window_config::HistoryWindowConfig;

#[cfg(test)]
#[path = "admission_test.rs"]
mod admission_test;

#[cfg(test)]
#[path = "anchor_test.rs"]
mod anchor_test;

#[cfg(test)]
#[path = "commit_test.rs"]
mod commit_test;

mod beta;
#[cfg(test)]
#[path = "index_test.rs"]
mod index_test;
pub use beta::{
    apply_history_window_beta, history_window_beta_enabled, history_window_beta_enabled_from_flags,
};

#[cfg(test)]
#[path = "model_test.rs"]
mod model_test;

#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;
