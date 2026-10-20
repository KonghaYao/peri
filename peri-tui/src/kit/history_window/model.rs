//! 窗口身份、段/缺口模型、预算账本与段登记。
//!
//! 账本口径：
//! - `resident_body_bytes`：已驻留段的**实际**正文字节（`String/Vec` 的 len 口径）；
//! - `resident_reserved_bytes`：同一批段的**容量下界**（capacity/容器开销估算），
//!   与 body 分开记录，不互相冒充；
//! - `page_reserved_bytes`：在途页预留（尚未委身为段），独立于 body；
//! - `pinned_body_bytes`：驻留段中不可淘汰部分（是 `resident_body_bytes` 的子集）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

use super::HistoryWindowConfig;
use super::budget::{INITIAL_CHECKPOINT_HISTORY, WindowBudget};
use super::detail::DetailDescriptor;
use super::diagnostics::{HistoryWindowDiagnostics, WindowStats};
use super::display::DisplayLedger;
use super::index::ResidentIndex;
pub(crate) use super::source::{HistorySource, PersistentEntryKey, PersistentSubItemKey};

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

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

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

    pub fn contains(&self, value: &T) -> bool {
        self.set.contains(value)
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

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
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }
}

/// 历史窗口：模型 + 账本 + 布局，不持有展示正文。
#[derive(Clone, Debug)]
pub struct HistoryWindow {
    enabled: bool,
    identity: Option<WindowIdentity>,
    generation: u64,
    pub(crate) layout: VecDeque<RegionKey>,
    pub(crate) segments: HashMap<SegmentKey, ResidentSegment>,
    pub(crate) gaps: HashMap<GapId, HistoryGap>,
    pub(crate) index: ResidentIndex,
    pub(crate) display: DisplayLedger,
    pub(crate) detail_descriptors: HashMap<PersistentEntryKey, DetailDescriptor>,
    pub(crate) budget: WindowBudget,
    pub(crate) anchor: Option<WindowAnchor>,
    durable_watermark: Option<DurableWatermark>,
    checkpoints_seen: BoundedSet<String>,
    pub(crate) reservations: HashMap<String, PageReservation>,
    pub(crate) pending_merge: Option<PendingMerge>,
    pub(crate) pending_release: Vec<ReleasePlan>,
    pub(crate) pending_release_epoch: u64,
    next_segment_seq: u64,
    next_gap_id: u64,
    access_clock: u64,
    stats: WindowStats,
    status: WindowStatus,
}

impl Default for HistoryWindow {
    fn default() -> Self {
        Self::new(HistoryWindowConfig::default())
    }
}

impl HistoryWindow {
    pub fn new(config: HistoryWindowConfig) -> Self {
        let limits = config.limits;
        let checkpoints_seen = BoundedSet::new(limits.checkpoint_history);
        let index = ResidentIndex::new();
        Self {
            enabled: config.history_window_v1,
            identity: None,
            generation: 0,
            layout: VecDeque::new(),
            segments: HashMap::new(),
            gaps: HashMap::new(),
            index,
            display: DisplayLedger::default(),
            detail_descriptors: HashMap::new(),
            budget: WindowBudget::new(limits),
            anchor: None,
            durable_watermark: None,
            checkpoints_seen,
            reservations: HashMap::new(),
            pending_merge: None,
            pending_release: Vec::new(),
            pending_release_epoch: 0,
            next_segment_seq: 0,
            next_gap_id: 0,
            access_clock: 0,
            stats: WindowStats::default(),
            status: WindowStatus::Ready,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn identity(&self) -> Option<&WindowIdentity> {
        self.identity.as_ref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn layout(&self) -> &VecDeque<RegionKey> {
        &self.layout
    }

    pub fn segments(&self) -> &HashMap<SegmentKey, ResidentSegment> {
        &self.segments
    }

    pub fn gaps(&self) -> &HashMap<GapId, HistoryGap> {
        &self.gaps
    }

    pub fn index(&self) -> &ResidentIndex {
        &self.index
    }

    pub fn budget(&self) -> &WindowBudget {
        &self.budget
    }

    pub fn anchor(&self) -> Option<&WindowAnchor> {
        self.anchor.as_ref()
    }

    pub fn durable_watermark(&self) -> Option<&DurableWatermark> {
        self.durable_watermark.as_ref()
    }

    pub fn stats(&self) -> WindowStats {
        self.stats
    }

    pub fn status(&self) -> &WindowStatus {
        &self.status
    }

    pub fn is_degraded(&self) -> bool {
        self.status.is_degraded()
    }

    /// 进入 Degraded：停止淘汰，保留现有可读段，等待上层重试 `open`。
    pub fn set_degraded(&mut self, reason: impl Into<String>) {
        self.status = WindowStatus::Degraded {
            reason: reason.into(),
        };
    }

    /// 只有成功的 `open`/身份对齐才能恢复 Ready。
    pub fn mark_ready(&mut self) {
        self.status = WindowStatus::Ready;
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn gap_count(&self) -> usize {
        self.gaps.len()
    }

    pub fn unit_count(&self) -> usize {
        self.index.total_units()
    }

    pub fn resident_entry_count(&self) -> usize {
        self.index.entry_count()
    }

    pub fn is_checkpoint_seen(&self, checkpoint_id: &str) -> bool {
        self.checkpoints_seen.contains(&checkpoint_id.to_string())
    }

    /// 记录 checkpoint；返回 `true` 表示首次看到（幂等去重）。
    pub fn note_checkpoint(&mut self, checkpoint_id: impl Into<String>) -> bool {
        self.checkpoints_seen.insert(checkpoint_id.into())
    }

    pub fn set_durable_watermark(&mut self, watermark: DurableWatermark) {
        self.durable_watermark = Some(watermark);
    }

    /// 开始/切换窗口身份。身份变化清空段、缺口、索引、预留与锚点并推进代次。
    ///
    /// 调用前必须用 `switch_snapshot` 读取旧展示区间并完成 `committed` 协调；
    /// 本函数会清空 index/display/segments。
    pub fn begin_window(&mut self, identity: WindowIdentity) -> u64 {
        if self.identity.as_ref() == Some(&identity) {
            return self.generation;
        }
        self.clear_state();
        self.generation = self.generation.saturating_add(1);
        self.identity = Some(identity);
        self.generation
    }

    /// 会话/rewind/reset 边界：清空窗口并推进代次。
    pub fn reset(&mut self) {
        self.clear_state();
        self.generation = self.generation.saturating_add(1);
        self.identity = None;
    }

    fn clear_state(&mut self) {
        self.layout.clear();
        self.segments.clear();
        self.gaps.clear();
        self.index = ResidentIndex::new();
        self.display.clear();
        self.detail_descriptors.clear();
        self.budget = WindowBudget::new(self.budget.limits.clone());
        self.anchor = None;
        self.durable_watermark = None;
        self.checkpoints_seen.clear();
        self.reservations.clear();
        self.pending_merge = None;
        self.pending_release.clear();
        self.pending_release_epoch = 0;
        self.next_segment_seq = 0;
        self.next_gap_id = 0;
        self.access_clock = 0;
        self.stats = WindowStats::default();
        self.status = WindowStatus::Ready;
    }

    pub fn segment(&self, key: SegmentKey) -> Option<&ResidentSegment> {
        self.segments.get(&key)
    }

    /// committed 可夹有未登记工具；插页坐标以展示账本而非 resident 前缀和为准。
    pub fn segment_display_bounds(&self, segment: SegmentKey) -> Option<(usize, usize)> {
        self.display
            .debug_ranges()
            .into_iter()
            .filter(|(_, key, _)| {
                self.index
                    .locate_entry(key)
                    .is_some_and(|locator| locator.segment == segment)
            })
            .map(|(start, _, len)| (start, start.saturating_add(len)))
            .reduce(|(start, end), (next_start, next_end)| {
                (start.min(next_start), end.max(next_end))
            })
    }

    /// 身份切换时仍须保留的 live 展示槽位，不把它们当成旧页摘除。
    pub fn live_seal_display_ranges(&self) -> Vec<(usize, usize)> {
        self.display
            .debug_ranges()
            .into_iter()
            .filter_map(|(start, key, len)| {
                let locator = self.index.locate_entry(&key)?;
                let segment = self.segments.get(&locator.segment)?;
                (segment.origin == SegmentOrigin::LiveSeal).then_some((start, len))
            })
            .collect()
    }

    /// 将页响应中同一持久身份的 LiveSeal 条目对齐到真实持久位置。
    /// 匹配不到时不改变账本；未提交前仍保持非 durable。
    pub fn promote_live_seal_position(&mut self, key: &PersistentEntryKey, position: u64) -> bool {
        let Some(locator) = self.index.locate_entry(key).copied() else {
            return false;
        };
        let Some(segment) = self.segments.get_mut(&locator.segment) else {
            return false;
        };
        if segment.origin != SegmentOrigin::LiveSeal {
            return false;
        }
        segment.entries[locator.ordinal].position = Some(position);
        self.display.insert(key.clone(), Some(position));
        true
    }

    pub fn region_index_of_segment(&self, key: SegmentKey) -> Option<usize> {
        self.layout
            .iter()
            .position(|region| matches!(region, RegionKey::Resident(segment) if *segment == key))
    }

    /// 登记一个驻留段。调用方保证段内容与 `committed` 的顺序由 `placement` 表达。
    pub fn register_segment(
        &mut self,
        draft: SegmentDraft,
        placement: SegmentPlacement,
    ) -> Result<SegmentKey, WindowModelError> {
        if draft.entries.is_empty() {
            return Err(WindowModelError::EmptySegment);
        }
        if draft
            .entries
            .iter()
            .any(|entry| entry.key.source != draft.source)
        {
            return Err(WindowModelError::MixedSource);
        }
        let key = SegmentKey {
            window_generation: self.generation,
            seq: self.next_segment_seq,
        };
        let mut pins = draft.pins;
        if !draft.durable {
            pins.insert(PinReason::Unpersisted);
        }
        if matches!(draft.source, HistorySource::Inherited { .. }) {
            pins.insert(PinReason::InheritedUnsupported);
        }
        let unit_count = draft.unit_count();
        let segment = ResidentSegment {
            key,
            source: draft.source,
            unit_count,
            entries: draft.entries,
            body_bytes: draft.body_bytes,
            reserved_bytes: draft.reserved_bytes,
            durable: draft.durable,
            sealed: draft.sealed,
            pins,
            last_access: self.access_clock,
            created_seq: key.seq,
            origin: draft.origin,
            before_cursor: draft.before_cursor,
            after_cursor: draft.after_cursor,
        };
        self.index.can_register(&segment)?;
        let position = match placement {
            SegmentPlacement::Tail => self.layout.len(),
            SegmentPlacement::BeforeSegment(anchor) => self
                .region_index_of_segment(anchor)
                .ok_or(WindowModelError::PlacementUnavailable)?,
            SegmentPlacement::AfterSegment(anchor) => self
                .region_index_of_segment(anchor)
                .map(|index| index + 1)
                .ok_or(WindowModelError::PlacementUnavailable)?,
        };
        self.layout.insert(position, RegionKey::Resident(key));
        self.index.register_segment(&segment);
        self.budget.resident_body_bytes += segment.body_bytes;
        self.budget.resident_reserved_bytes += segment.reserved_bytes;
        self.segments.insert(key, segment);
        self.next_segment_seq = self.next_segment_seq.saturating_add(1);
        self.rebuild_slots();
        self.refresh_pinned_budget();
        Ok(key)
    }

    /// 模型层摘除段（不创建 gap、不删除渲染对象）。返回段与它在布局中的原位置。
    pub(crate) fn detach_segment(&mut self, key: SegmentKey) -> Option<(ResidentSegment, usize)> {
        let position = self.region_index_of_segment(key)?;
        self.layout.remove(position)?;
        let segment = self.segments.remove(&key)?;
        for entry in &segment.entries {
            self.detail_descriptors.remove(&entry.key);
        }
        self.index.remove_segment(key);
        self.budget.resident_body_bytes = self
            .budget
            .resident_body_bytes
            .saturating_sub(segment.body_bytes);
        self.budget.resident_reserved_bytes = self
            .budget
            .resident_reserved_bytes
            .saturating_sub(segment.reserved_bytes);
        self.rebuild_slots();
        self.refresh_pinned_budget();
        Some((segment, position))
    }

    pub fn pin(&mut self, key: SegmentKey, reason: PinReason) {
        let became_pinned = if let Some(segment) = self.segments.get_mut(&key) {
            let was_empty = segment.pins.is_empty();
            segment.pins.insert(reason);
            was_empty && !segment.pins.is_empty()
        } else {
            return;
        };
        if became_pinned {
            self.index.set_segment_pinned(key, true);
        }
        self.refresh_pinned_budget();
    }

    pub fn unpin(&mut self, key: SegmentKey, reason: PinReason) {
        let became_unpinned = if let Some(segment) = self.segments.get_mut(&key) {
            segment.pins.remove(reason);
            segment.pins.is_empty()
        } else {
            return;
        };
        if became_unpinned {
            self.index.set_segment_pinned(key, false);
        }
        self.refresh_pinned_budget();
    }

    pub fn touch_segment(&mut self, key: SegmentKey) {
        self.access_clock = self.access_clock.saturating_add(1);
        if let Some(segment) = self.segments.get_mut(&key) {
            segment.last_access = self.access_clock;
        }
    }

    pub fn access_clock(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    /// 重新计算布局的槽位前缀和。段顺序必须与 `committed` 顺序一致。
    pub fn rebuild_slots(&mut self) {
        let order: Vec<SegmentKey> = self
            .layout
            .iter()
            .filter_map(|region| match region {
                RegionKey::Resident(key) => Some(*key),
                RegionKey::Gap(_) => None,
            })
            .collect();
        self.index.rebuild_slots(&order, &self.segments);
    }

    fn refresh_pinned_budget(&mut self) {
        self.budget.pinned_body_bytes = self
            .segments
            .values()
            .filter(|segment| segment.pins.is_pinned())
            .map(|segment| segment.body_bytes)
            .sum();
    }

    pub(crate) fn next_gap_id(&mut self) -> GapId {
        let id = GapId(self.next_gap_id);
        self.next_gap_id = self.next_gap_id.saturating_add(1);
        id
    }

    /// 摘除段之前先算好最近的前后驻留条目边界。
    pub(crate) fn neighbor_entries(
        &self,
        key: SegmentKey,
    ) -> (Option<PersistentEntryKey>, Option<PersistentEntryKey>) {
        let Some(index) = self.region_index_of_segment(key) else {
            return (None, None);
        };
        let before = self
            .layout
            .iter()
            .take(index)
            .rev()
            .find_map(|region| match region {
                RegionKey::Resident(segment) => self
                    .segments
                    .get(segment)
                    .and_then(|segment| segment.last_entry())
                    .map(|entry| entry.key.clone()),
                RegionKey::Gap(_) => None,
            });
        let after = self
            .layout
            .iter()
            .skip(index + 1)
            .find_map(|region| match region {
                RegionKey::Resident(segment) => self
                    .segments
                    .get(segment)
                    .and_then(|segment| segment.first_entry())
                    .map(|entry| entry.key.clone()),
                RegionKey::Gap(_) => None,
            });
        (before, after)
    }

    pub fn take_pending_release_plans(&mut self) -> Vec<ReleasePlan> {
        std::mem::take(&mut self.pending_release)
    }

    pub(crate) fn record_release_plan(&mut self, plan: ReleasePlan) {
        self.pending_release.push(plan);
    }

    pub fn diagnostics(&self) -> HistoryWindowDiagnostics {
        HistoryWindowDiagnostics {
            tui_history_body_bytes: self.budget.resident_body_bytes,
            tui_history_reserved_bytes: self.budget.resident_reserved_bytes,
            tui_pinned_bytes: self.budget.pinned_body_bytes,
            history_page_reserved_bytes: self.budget.page_reserved_bytes,
            history_page_pending_bytes: self.budget.pending_merge_bytes,
            detail_window_bytes: self.budget.detail_bytes,
            resident_segments: self.segments.len(),
            gap_count: self.gaps.len(),
            evicted_bytes: self.stats.evicted_bytes,
            evict_blocked_pinned: self.stats.evict_blocked_pinned,
            admission_blocked: self.stats.admission_blocked,
            stale_pages: self.stats.stale_pages,
            window_degraded: self.is_degraded(),
        }
    }

    pub(crate) fn stats_mut(&mut self) -> &mut WindowStats {
        &mut self.stats
    }
}
