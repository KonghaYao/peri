//! 窗口身份、段/缺口模型、预算账本与段登记。
//!
//! 账本口径：
//! - `resident_body_bytes`：已驻留段的**实际**正文字节（`String/Vec` 的 len 口径）；
//! - `resident_reserved_bytes`：同一批段的**容量下界**（capacity/容器开销估算），
//!   与 body 分开记录，不互相冒充；
//! - `page_reserved_bytes`：在途页预留（尚未委身为段），独立于 body；
//! - `pinned_body_bytes`：驻留段中不可淘汰部分（是 `resident_body_bytes` 的子集）。

use std::collections::{HashSet, VecDeque};
use std::ops::Range;

use super::super::budget::INITIAL_CHECKPOINT_HISTORY;
use super::super::source::{HistorySource, PersistentEntryKey, PersistentSubItemKey};

/// 本地生命周期句柄：`window_generation + seq`。
///
/// 不是消息身份，不进入 wire、锚点、工具配对或持久化；窗口换代后旧键全部作废。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SegmentKey {
    pub window_generation: u64,
    pub seq: u64,
}

/// 浏览身份：会话 + 会话代 + 结构版本 + 快照。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WindowIdentity {
    pub session_id: String,
    pub session_generation: String,
    pub structure_version: u64,
    pub snapshot_token: String,
}

impl WindowIdentity {
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        session_generation: impl Into<String>,
        structure_version: u64,
        snapshot_token: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            session_generation: session_generation.into(),
            structure_version,
            snapshot_token: snapshot_token.into(),
        }
    }
}

/// 最近一次持久提交水位（不透明 wire 值；S5 只保存，不解释）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DurableWatermark {
    pub checkpoint_id: Option<String>,
    pub committed_tail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentOrigin {
    Replay,
    Page,
    LiveSeal,
}

/// 固定原因。非持久化/在途/运行中的段必须至少带一个原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PinReason {
    Viewport,
    Unpersisted,
    Running,
    DurablePending,
    InheritedUnsupported,
    DetailOpen,
    AnchorMissing,
}

impl PinReason {
    pub const ALL: [PinReason; 7] = [
        Self::Viewport,
        Self::Unpersisted,
        Self::Running,
        Self::DurablePending,
        Self::InheritedUnsupported,
        Self::DetailOpen,
        Self::AnchorMissing,
    ];

    #[must_use]
    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// 固定原因集合（手写位集，避免新增依赖）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PinReasons(u16);

impl PinReasons {
    pub const fn none() -> Self {
        Self(0)
    }

    pub fn insert(&mut self, reason: PinReason) {
        self.0 |= reason.bit();
    }

    pub fn remove(&mut self, reason: PinReason) {
        self.0 &= !reason.bit();
    }

    pub fn contains(&self, reason: PinReason) -> bool {
        self.0 & reason.bit() != 0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub fn is_pinned(&self) -> bool {
        !self.is_empty()
    }

    pub fn reasons(&self) -> Vec<PinReason> {
        PinReason::ALL
            .into_iter()
            .filter(|reason| self.contains(*reason))
            .collect()
    }
}

/// 段内一个条目的身份与计量（不持有正文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntryView {
    pub key: PersistentEntryKey,
    /// 仅 own 来源有 `position`；继承来源没有。
    pub position: Option<u64>,
    pub unit_count: usize,
    pub body_bytes: usize,
    pub reserved_bytes: usize,
    pub sub_items: Vec<PersistentSubItemKey>,
}

impl HistoryEntryView {
    #[must_use]
    pub fn new(
        key: PersistentEntryKey,
        position: Option<u64>,
        unit_count: usize,
        body_bytes: usize,
        reserved_bytes: usize,
        sub_items: Vec<PersistentSubItemKey>,
    ) -> Self {
        Self {
            key,
            position,
            unit_count,
            body_bytes,
            reserved_bytes,
            sub_items,
        }
    }
}

/// 驻留段：只保存持久身份、单位计数和字节账目，不复制 `TuiRenderUnit`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentSegment {
    pub key: SegmentKey,
    pub source: HistorySource,
    pub entries: Vec<HistoryEntryView>,
    pub unit_count: usize,
    pub body_bytes: usize,
    pub reserved_bytes: usize,
    pub durable: bool,
    /// 已被持久提交水位封印；封印不等于释放。
    pub sealed: bool,
    pub pins: PinReasons,
    pub last_access: u64,
    pub created_seq: u64,
    pub origin: SegmentOrigin,
    /// 段的页边界游标：淘汰成 gap 后作为重读入口。
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
}

impl ResidentSegment {
    pub fn first_entry(&self) -> Option<&HistoryEntryView> {
        self.entries.first()
    }

    pub fn last_entry(&self) -> Option<&HistoryEntryView> {
        self.entries.last()
    }

    pub fn entry_keys(&self) -> Vec<PersistentEntryKey> {
        self.entries.iter().map(|entry| entry.key.clone()).collect()
    }
}

/// 段登记输入。`unit_count` 由条目求和，避免调用方与模型各算一份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentDraft {
    pub source: HistorySource,
    pub entries: Vec<HistoryEntryView>,
    pub body_bytes: usize,
    pub reserved_bytes: usize,
    pub durable: bool,
    pub sealed: bool,
    pub pins: PinReasons,
    pub origin: SegmentOrigin,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
}

impl SegmentDraft {
    #[must_use]
    pub fn new(
        source: HistorySource,
        entries: Vec<HistoryEntryView>,
        body_bytes: usize,
        reserved_bytes: usize,
    ) -> Self {
        Self {
            source,
            entries,
            body_bytes,
            reserved_bytes,
            durable: true,
            sealed: false,
            pins: PinReasons::none(),
            origin: SegmentOrigin::Page,
            before_cursor: None,
            after_cursor: None,
        }
    }

    pub fn durable(mut self, durable: bool) -> Self {
        self.durable = durable;
        self
    }

    pub fn origin(mut self, origin: SegmentOrigin) -> Self {
        self.origin = origin;
        self
    }

    pub fn pinned(mut self, reason: PinReason) -> Self {
        self.pins.insert(reason);
        self
    }

    pub fn cursors(mut self, before: Option<String>, after: Option<String>) -> Self {
        self.before_cursor = before;
        self.after_cursor = after;
        self
    }

    pub fn unit_count(&self) -> usize {
        self.entries.iter().map(|entry| entry.unit_count).sum()
    }
}

/// 段在布局中的插入位置。`SegmentKey` 只在本窗口代内有效。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentPlacement {
    Tail,
    BeforeSegment(SegmentKey),
    AfterSegment(SegmentKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GapId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapPlaceholder {
    OlderHistory,
    MissingMiddle,
    DetailUnavailable,
}

impl GapPlaceholder {
    pub(crate) fn merge(self, other: Self) -> Self {
        use GapPlaceholder::*;
        if self == MissingMiddle || other == MissingMiddle {
            MissingMiddle
        } else if self == OlderHistory || other == OlderHistory {
            OlderHistory
        } else {
            DetailUnavailable
        }
    }
}

/// 缺口只保存边界与游标，不保存逐条目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryGap {
    pub id: GapId,
    pub before: Option<PersistentEntryKey>,
    pub after: Option<PersistentEntryKey>,
    /// 从缺口上端向下补：更晚邻居段的 `before_cursor`。
    pub before_cursor: Option<String>,
    /// 从缺口下端向上补：更早邻居段的 `after_cursor`。
    pub after_cursor: Option<String>,
    pub source: HistorySource,
    pub placeholder: GapPlaceholder,
}

impl HistoryGap {
    #[must_use]
    pub fn new(
        id: GapId,
        before: Option<PersistentEntryKey>,
        after: Option<PersistentEntryKey>,
        before_cursor: Option<String>,
        after_cursor: Option<String>,
        source: HistorySource,
        placeholder: GapPlaceholder,
    ) -> Self {
        Self {
            id,
            before,
            after,
            before_cursor,
            after_cursor,
            source,
            placeholder,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKey {
    Resident(SegmentKey),
    Gap(GapId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowAnchorOffset {
    CharOffset(u64),
    BlockOffset(u32),
}

/// 选择锚点：只保存稳定身份与局部偏移。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionAnchor {
    pub entry: PersistentEntryKey,
    pub sub_item: Option<u32>,
    pub char_offset: u64,
}

/// 跨淘汰/跨窗口的持久定位锚点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowAnchor {
    pub entry: PersistentEntryKey,
    pub sub_item: Option<u32>,
    pub offset: WindowAnchorOffset,
    pub selection: Option<SelectionAnchor>,
    pub follow_bottom: bool,
}

impl WindowAnchor {
    #[must_use]
    pub fn new(
        entry: PersistentEntryKey,
        sub_item: Option<u32>,
        offset: WindowAnchorOffset,
    ) -> Self {
        Self {
            entry,
            sub_item,
            offset,
            selection: None,
            follow_bottom: false,
        }
    }

    pub fn follow_bottom(mut self, follow_bottom: bool) -> Self {
        self.follow_bottom = follow_bottom;
        self
    }

    pub fn with_selection(mut self, selection: SelectionAnchor) -> Self {
        self.selection = Some(selection);
        self
    }
}

/// 淘汰决策。只描述应释放的段/键与账目；不持有、也不删除渲染对象。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleasePlan {
    pub segment: SegmentKey,
    pub source: HistorySource,
    pub entries: Vec<PersistentEntryKey>,
    pub unit_count: usize,
    pub body_bytes: usize,
    pub reserved_bytes: usize,
    /// 段在 `committed` 中的槽位区间；本阶段为模型前缀和，不用于渲染侧计算。
    pub slot_range: Option<Range<usize>>,
    pub before: Option<PersistentEntryKey>,
    pub after: Option<PersistentEntryKey>,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseReason {
    BudgetPressure,
    Reset,
    Commit,
}

/// 有界集合：最近 N 个 checkpointId。超出容量按插入顺序淘汰最旧值。
#[derive(Clone, Debug)]
pub struct BoundedSet<T> {
    capacity: usize,
    order: VecDeque<T>,
    set: HashSet<T>,
}

impl<T> Default for BoundedSet<T> {
    fn default() -> Self {
        Self {
            capacity: INITIAL_CHECKPOINT_HISTORY,
            order: VecDeque::new(),
            set: HashSet::new(),
        }
    }
}

impl<T: Clone + Eq + std::hash::Hash> BoundedSet<T> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: VecDeque::new(),
            set: HashSet::new(),
        }
    }

    pub fn insert(&mut self, value: T) -> bool {
        if !self.set.insert(value.clone()) {
            return false;
        }
        self.order.push_back(value);
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
        true
    }

    #[must_use]
    pub fn contains(&self, value: &T) -> bool {
        self.set.contains(value)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.set.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.set.clear();
    }
}

/// 页预留：容量下界，接纳时结算并转入段账本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageReservation {
    pub request_id: String,
    pub window_generation: u64,
    pub bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingMerge {
    pub request_id: String,
    pub body_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WindowModelError {
    #[error("segment source does not match entry source")]
    MixedSource,
    #[error("duplicate resident entry {0:?}")]
    DuplicateEntry(PersistentEntryKey),
    #[error("duplicate resident sub item {0:?}")]
    DuplicateSubItem(PersistentSubItemKey),
    #[error("segment has no display units")]
    EmptySegment,
    #[error("segment placement is not present in this window generation")]
    PlacementUnavailable,
}

/// 窗口运行状态。Degraded 期间停止淘汰、保留可读内容，等待 `open` 重试。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WindowStatus {
    #[default]
    Ready,
    Degraded {
        reason: String,
    },
}

impl WindowStatus {
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }
}
