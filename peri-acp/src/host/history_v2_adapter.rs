//! History V2 的本地历史适配器（H3a2：own + inherited 冻结前缀）。
//!
//! 复用 `SessionResources` 既有的有界窗口读取端口，不再全量物化历史：
//!
//! - open/page/seek 使用 [`HistoryWindowPagePlan`]：固定 own tail、keyset 区间、字节预算，
//!   超预算条目只返回 descriptor 候选，不取正文；
//! - 逻辑顺序固定为 inherited 冻结区 → own canonical 区；跨区页由适配器组合两次有界读取，
//!   不新增存储结构，也不把 inherited 回退成父会话当前内容；
//! - body 按描述符的有界区间读取（SQLite blob 范围读），inherited 走既有冻结快照分支；
//! - 展示投影直接复用 [`dispatch::project_payload_updates`] 与
//!   [`dispatch::reminder_visible_to_client`]，V2 不复制另一套展示规则。
//!
//! `historyRevision` 是 own 存储序 tail position，不是持久结构版本；inherited-only 页仍
//! 沿用会话 own tail。跨实例失效不在本后端承诺内。

mod body;
mod display;
mod open;
mod page;
mod projection;
mod refs;
mod view;

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::history_common::{
    HistoryBodyChunk as WindowBodyChunk, HistoryCommittedTail, HistoryEntryId, HistoryReadTicket,
    HistorySource as WindowSource, HistoryWindowDirection, HistoryWindowError,
    HISTORY_WINDOW_MAX_BYTE_BUDGET,
};
use peri_acp_types::history_v2::{
    DecimalU64, HistoryAnchor, HistoryBackendError, HistoryBodySlice, HistoryBoundary,
    HistoryBoundaryPosition, HistoryBoundarySide, HistoryCommittedTailRef, HistoryCoverage,
    HistoryCoverageInterval, HistoryDirection, HistoryDurability, HistoryDurabilityState,
    HistoryEntry, HistoryEntryAnchor, HistoryOpenedSnapshot, HistoryOrder, HistoryOrigin,
    HistoryPageData, HistoryProgress, HistoryProgressReason, HistorySnapshotRef,
    HistorySource as V2Source, HistoryV2Backend, HISTORY_V2_PROJECTION_VERSION,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    HistoryEntryMetadata, HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent,
    HistoryWindowRows, HistoryWindowTailExpectation, InheritedHistoryEntry,
    SessionHistoryWindowBounds, SessionResourceError, SessionResourceErrorKind, SessionResources,
};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::PeriCaps;

use crate::dispatch;

#[cfg(test)]
pub(crate) use self::projection::PrefixVerdict;
pub(crate) use self::projection::PrefixVisibility;
use self::refs::{EntryRef, InheritedRef, INHERITED_REGION, OWN_REGION};
use self::view::undetermined_visibility;

/// 描述符默认块大小（设计 §10.5：默认 64 KiB）。
const DEFAULT_CHUNK_BYTES: u32 = 64 * 1024;

/// 测试探针：直接验证前缀判定纯函数（真实存储无法覆盖所有畸形 payload，
/// 因此仅在此暴露，不改变生产路径）。
#[cfg(test)]
pub(crate) fn inherited_prefix_verdict(prefix: &[u8]) -> PrefixVerdict {
    projection::prefix_verdict(prefix)
}

/// 用现有窗口读取端口实现 History V2 `HistoryV2Backend` 的本地适配器。
pub(crate) struct HistoryV2BackendAdapter {
    resources: Arc<dyn SessionResources>,
    caps: PeriCaps,
    max_body_chunk_bytes: u32,
    #[cfg(test)]
    visibility_reads: parking_lot::Mutex<Vec<u32>>,
    /// 测试探针：注入前缀读取失败（模拟瞬态 I/O），验证页面失败而不是把条目当隐藏。
    #[cfg(test)]
    visibility_read_failures: std::sync::atomic::AtomicUsize,
    /// 测试探针：记录 body 读取 canonical payload 时实际请求的字节数。
    #[cfg(test)]
    payload_reads: parking_lot::Mutex<Vec<u32>>,
}

/// 单次 region 读取的有界结果；own 与 inherited 共用同一形状。
struct RegionView {
    entries: Vec<HistoryEntry>,
    before_anchor: Option<MessageId>,
    after_anchor: Option<MessageId>,
    /// 本页实际返回的最旧 / 最新可见条目；跨区续页时作为「已消费」游标。
    oldest: Option<MessageId>,
    newest: Option<MessageId>,
    before_more: bool,
    after_more: bool,
    raw_count: usize,
    used_bytes: usize,
    committed_tail: Option<HistoryCommittedTail>,
}

/// 组装一页所需的全部事实；coverage 由 entries 与两个方向的真实边界推导。
struct PageParts {
    entries: Vec<HistoryEntry>,
    before_cursor: Option<String>,
    after_cursor: Option<String>,
    before_more: bool,
    after_more: bool,
    committed_tail: Option<HistoryCommittedTailRef>,
    history_revision: DecimalU64,
}

impl HistoryV2BackendAdapter {
    pub(crate) fn new(resources: Arc<dyn SessionResources>, caps: PeriCaps) -> Self {
        let max_body_chunk_bytes = caps
            .history_v2
            .as_ref()
            .map(|capability| capability.limits.max_body_chunk_bytes)
            .unwrap_or(peri_acp_types::history_v2::HISTORY_DEFAULT_BODY_CHUNK_BYTES);
        Self {
            resources,
            caps,
            max_body_chunk_bytes: max_body_chunk_bytes.max(1),
            #[cfg(test)]
            visibility_reads: parking_lot::Mutex::new(Vec::new()),
            #[cfg(test)]
            visibility_read_failures: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            payload_reads: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// 测试探针：记录 oversize 可见性判定实际请求的前缀上限。
    #[cfg(test)]
    pub(crate) fn visibility_read_max_bytes(&self) -> Vec<u32> {
        self.visibility_reads.lock().clone()
    }

    #[cfg(test)]
    pub(crate) fn reset_visibility_reads(&self) {
        self.visibility_reads.lock().clear();
    }

    /// 测试探针：让接下来的 `count` 次前缀读取返回瞬态错误。
    #[cfg(test)]
    pub(crate) fn inject_visibility_read_failures(&self, count: usize) {
        self.visibility_read_failures
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn visibility_read_fails(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.visibility_read_failures
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    /// 测试探针：body 为拼接 canonical payload 实际请求的分块大小。
    #[cfg(test)]
    pub(crate) fn payload_read_max_bytes(&self) -> Vec<u32> {
        self.payload_reads.lock().clone()
    }

    fn chunk_bytes(&self) -> u32 {
        DEFAULT_CHUNK_BYTES.min(self.max_body_chunk_bytes).max(1)
    }

    async fn load_rows(
        &self,
        id: &ThreadId,
        plan: &HistoryWindowPagePlan,
    ) -> Result<HistoryWindowRows, HistoryBackendError> {
        self.resources
            .load_history_window_page(id, plan)
            .await
            .map_err(|error| map_resource_error(&error))
    }

    async fn load_body(
        &self,
        id: &ThreadId,
        source: &WindowSource,
        entry_id: MessageId,
        offset: u64,
        max_bytes: u32,
        tail: Option<&HistoryCommittedTail>,
    ) -> Result<WindowBodyChunk, HistoryBackendError> {
        self.resources
            .load_history_body_chunk(
                id,
                source.clone(),
                entry_id,
                offset,
                max_bytes,
                tail,
                // 票据由 H2 负责授权；存储实现不解释该值，这里只提供一个非空占位。
                &HistoryReadTicket::new("history-v2"),
            )
            .await
            .map_err(|error| map_resource_error(&error))
    }

    /// body 定位的独立元数据读取：长度 + own role，不依赖页面预算或内联状态。
    async fn entry_metadata(
        &self,
        id: &ThreadId,
        source: &WindowSource,
        entry_id: MessageId,
        tail: Option<&HistoryCommittedTail>,
    ) -> Result<HistoryEntryMetadata, HistoryBackendError> {
        self.resources
            .read_history_entry_metadata(
                id,
                source.clone(),
                entry_id,
                tail,
                &HistoryReadTicket::new("history-v2"),
            )
            .await
            .map_err(|error| map_resource_error(&error))
    }

    async fn bounds(
        &self,
        id: &ThreadId,
    ) -> Result<SessionHistoryWindowBounds, HistoryBackendError> {
        self.resources
            .history_window_bounds(id)
            .await
            .map_err(|error| map_resource_error(&error))
    }

    fn inherited_ref(
        bounds: &SessionHistoryWindowBounds,
    ) -> Result<Option<InheritedRef>, HistoryBackendError> {
        let Some(snapshot) = bounds.inherited.snapshot() else {
            return Ok(None);
        };
        InheritedRef::from_snapshot(snapshot)
            .map(Some)
            .ok_or(HistoryBackendError::SourceUnavailable)
    }

    /// page/seek/body 读取 inherited 时补做固定 own tail 复核。
    ///
    /// inherited 读取本身只验证冻结快照；但窗口固定的 own tail 被 rewind/精确删除后，
    /// 整个窗口都必须失效，不能因为本次只读 inherited 就继续成功。
    async fn ensure_snapshot_tail(
        &self,
        thread_id: &ThreadId,
        snapshot: &HistorySnapshotRef,
    ) -> Result<(), HistoryBackendError> {
        let Some(tail) = snapshot_tail(snapshot, thread_id)? else {
            return Ok(());
        };
        let budget = target_bytes(snapshot)?
            .max(1)
            .min(HISTORY_WINDOW_MAX_BYTE_BUDGET as usize);
        let plan = HistoryWindowPagePlan {
            intent: HistoryWindowReadIntent::Page,
            source: WindowSource::Own,
            anchor: None,
            direction: HistoryWindowDirection::Before,
            include_anchor: false,
            before_limit: 0,
            after_limit: 0,
            byte_budget: budget,
            tail: HistoryWindowTailExpectation::Fixed(Some(tail)),
        };
        self.load_rows(thread_id, &plan).await?;
        Ok(())
    }

    fn assemble_page(&self, parts: PageParts) -> HistoryPageData {
        let coverage = coverage(&parts.entries, parts.before_more, parts.after_more);
        HistoryPageData {
            progress: completed_progress(parts.entries.len()),
            entries: parts.entries,
            coverage,
            before_cursor: parts.before_cursor,
            after_cursor: parts.after_cursor,
            committed_tail: parts.committed_tail,
            history_revision: parts.history_revision,
            projection_version: HISTORY_V2_PROJECTION_VERSION,
        }
    }
}

#[async_trait]
impl HistoryV2Backend for HistoryV2BackendAdapter {
    async fn open(
        &self,
        session_id: &str,
        anchor: &HistoryAnchor,
        before: u32,
        after: u32,
        target_bytes: u64,
    ) -> Result<HistoryOpenedSnapshot, HistoryBackendError> {
        let thread_id = parse_thread(session_id)?;
        let target_bytes =
            usize::try_from(target_bytes).map_err(|_| HistoryBackendError::BudgetExceeded)?;
        match anchor {
            HistoryAnchor::Latest => {
                self.open_latest(&thread_id, before as usize, target_bytes)
                    .await
            }
            HistoryAnchor::Entry(entry) => {
                self.open_entry(&thread_id, entry, before, after, target_bytes)
                    .await
            }
        }
    }

    async fn page(
        &self,
        snapshot: &HistorySnapshotRef,
        raw_cursor: &str,
        direction: HistoryDirection,
        limit: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        let thread_id = parse_thread(&snapshot.session_id)?;
        match refs::parse_entry_ref(raw_cursor).ok_or(HistoryBackendError::InvalidCursor)? {
            EntryRef::Own(anchor) => {
                self.page_own(&thread_id, snapshot, anchor, direction, limit)
                    .await
            }
            EntryRef::Inherited { source, entry_id } => {
                self.page_inherited(&thread_id, snapshot, &source, entry_id, direction, limit)
                    .await
            }
        }
    }

    async fn seek(
        &self,
        snapshot: &HistorySnapshotRef,
        anchor: &HistoryEntryAnchor,
        before: u32,
        after: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        let thread_id = parse_thread(&snapshot.session_id)?;
        match &anchor.source {
            V2Source::Own => {
                self.seek_own(&thread_id, snapshot, anchor, before, after)
                    .await
            }
            V2Source::Inherited(source) => {
                let source =
                    InheritedRef::from_v2(source).ok_or(HistoryBackendError::SourceUnavailable)?;
                self.seek_inherited(&thread_id, snapshot, &source, anchor, before, after)
                    .await
            }
        }
    }

    async fn body(
        &self,
        snapshot: &HistorySnapshotRef,
        body_ref: &str,
        offset: u64,
        max_bytes: u64,
    ) -> Result<HistoryBodySlice, HistoryBackendError> {
        let thread_id = parse_thread(&snapshot.session_id)?;
        match refs::parse_entry_ref(body_ref).ok_or(HistoryBackendError::NotFound)? {
            EntryRef::Own(entry_id) => {
                let tail = snapshot_tail(snapshot, &thread_id)?
                    .ok_or(HistoryBackendError::AnchorUnavailable)?;
                // 正文定位只依赖稳定条目身份与固定 tail；页是否内联、页预算如何拆分
                // 都不参与判断。
                let metadata = self
                    .entry_metadata(&thread_id, &WindowSource::Own, entry_id, Some(&tail))
                    .await?;
                // page 过滤不是授权：body 必须按同一 role 口径再判一次。
                match self
                    .oversized_visibility(
                        &thread_id,
                        Some(&tail),
                        entry_id,
                        metadata.role,
                        metadata.canonical_bytes,
                    )
                    .await?
                {
                    PrefixVisibility::Visible => {}
                    PrefixVisibility::Hidden => return Err(HistoryBackendError::NotFound),
                    PrefixVisibility::Undetermined => {
                        return Err(undetermined_visibility(&thread_id, entry_id));
                    }
                }
                let canonical = self
                    .read_canonical_payload(
                        &thread_id,
                        &WindowSource::Own,
                        entry_id,
                        Some(&tail),
                        metadata.canonical_bytes,
                    )
                    .await?;
                self.body_slice(body_ref, offset, max_bytes, &canonical)
            }
            EntryRef::Inherited { source, entry_id } => {
                self.ensure_snapshot_tail(&thread_id, snapshot).await?;
                let metadata = self
                    .entry_metadata(&thread_id, &source.window_source(), entry_id, None)
                    .await?;
                // inherited 没有 role 列：用 ≤1024 字节前缀复核受众，无法判定时失败关闭。
                match self
                    .inherited_oversized_visibility(
                        &thread_id,
                        &source,
                        entry_id,
                        metadata.canonical_bytes,
                    )
                    .await?
                {
                    PrefixVisibility::Visible => {}
                    PrefixVisibility::Hidden => return Err(HistoryBackendError::NotFound),
                    PrefixVisibility::Undetermined => {
                        return Err(undetermined_visibility(&thread_id, entry_id));
                    }
                }
                let canonical = self
                    .read_canonical_payload(
                        &thread_id,
                        &source.window_source(),
                        entry_id,
                        None,
                        metadata.canonical_bytes,
                    )
                    .await?;
                self.body_slice(body_ref, offset, max_bytes, &canonical)
            }
        }
    }
}

fn inherited_seek_plan(
    source: &InheritedRef,
    entry_id: MessageId,
    before_limit: usize,
    after_limit: usize,
    byte_budget: usize,
) -> HistoryWindowPagePlan {
    HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source: source.window_source(),
        anchor: Some(entry_id),
        direction: HistoryWindowDirection::Around,
        include_anchor: true,
        before_limit,
        after_limit,
        byte_budget,
        // inherited 读取只认冻结快照；own tail 由 ensure_snapshot_tail 单独复核。
        tail: HistoryWindowTailExpectation::Fixed(None),
    }
}

fn window_entry_bytes(entry: &HistoryWindowEntry) -> usize {
    match entry {
        HistoryWindowEntry::Loaded(item) => item.encoded_bytes,
        HistoryWindowEntry::Oversized { total_bytes, .. } => {
            usize::try_from(*total_bytes).unwrap_or(usize::MAX)
        }
    }
}

fn inherited_entry_bytes(entry: &InheritedHistoryEntry) -> usize {
    match entry {
        InheritedHistoryEntry::Loaded(item) => item.encoded_bytes,
        InheritedHistoryEntry::Oversized { total_bytes, .. } => {
            usize::try_from(*total_bytes).unwrap_or(usize::MAX)
        }
    }
}

fn parse_thread(session_id: &str) -> Result<ThreadId, HistoryBackendError> {
    if session_id.is_empty() {
        return Err(HistoryBackendError::NotFound);
    }
    Ok(session_id.to_owned())
}

fn parse_message_id(raw: &str) -> Option<MessageId> {
    uuid::Uuid::parse_str(raw).ok().map(MessageId::from)
}

/// 接受 `dispatch::entry_id` 的 `{session}:{message}` 形式，也接受裸 message id。
fn parse_entry_id(raw: &str, thread_id: &ThreadId) -> Result<MessageId, HistoryBackendError> {
    if let Some(message_id) = parse_message_id(raw) {
        return Ok(message_id);
    }
    HistoryEntryId::parse_for_session(raw, thread_id)
        .map_err(|_| HistoryBackendError::AnchorUnavailable)
}

fn target_bytes(snapshot: &HistorySnapshotRef) -> Result<usize, HistoryBackendError> {
    usize::try_from(snapshot.target_bytes).map_err(|_| HistoryBackendError::BudgetExceeded)
}

fn snapshot_tail(
    snapshot: &HistorySnapshotRef,
    thread_id: &ThreadId,
) -> Result<Option<HistoryCommittedTail>, HistoryBackendError> {
    snapshot
        .committed_tail
        .as_ref()
        .map(|tail| {
            let entry_id = parse_entry_id(&tail.entry_id, thread_id)?;
            Ok(HistoryCommittedTail::new(
                thread_id,
                entry_id,
                tail.position,
            ))
        })
        .transpose()
}

fn committed_tail_ref(session_id: &str, tail: &HistoryCommittedTail) -> HistoryCommittedTailRef {
    HistoryCommittedTailRef {
        entry_id: dispatch::entry_id(session_id, tail.entry_id),
        position: tail.position,
    }
}

fn tail_refs(
    session_id: &str,
    tail: Option<&HistoryCommittedTail>,
) -> (Option<HistoryCommittedTailRef>, DecimalU64) {
    match tail {
        Some(tail) => (
            Some(committed_tail_ref(session_id, tail)),
            DecimalU64::new(tail.position),
        ),
        None => (None, DecimalU64::ZERO),
    }
}

fn own_durability(position: u64) -> HistoryDurability {
    HistoryDurability {
        state: HistoryDurabilityState::Sealed,
        origin: HistoryOrigin::Live,
        final_item_seq: Some(DecimalU64::new(position)),
    }
}

fn imported_durability() -> HistoryDurability {
    HistoryDurability {
        state: HistoryDurabilityState::Sealed,
        origin: HistoryOrigin::Imported,
        final_item_seq: None,
    }
}

fn own_order(position: u64) -> HistoryOrder {
    HistoryOrder {
        region: OWN_REGION.to_owned(),
        ordinal: DecimalU64::new(position),
    }
}

fn inherited_order(ordinal: u64) -> HistoryOrder {
    HistoryOrder {
        region: INHERITED_REGION.to_owned(),
        ordinal: DecimalU64::new(ordinal),
    }
}

fn completed_progress(consumed: usize) -> HistoryProgress {
    HistoryProgress {
        reason: HistoryProgressReason::Complete,
        scanned_entries: consumed as u64,
        consumed_entries: consumed as u64,
    }
}

/// V1 计划在 `include_anchor=true` 时把锚点计入返回预算：`before > 0` 时锚点由
/// before 半段包含，`before == 0` 时改由 after 半段包含。V2 的 before/after 只数
/// 锚点两侧，因此对应半段各加 1，锚点仍恰好出现一次。
fn anchor_limits(before: u32, after: u32) -> (usize, usize) {
    if before == 0 {
        (0, after as usize + 1)
    } else {
        (before as usize + 1, after as usize)
    }
}

fn coverage(entries: &[HistoryEntry], before_more: bool, after_more: bool) -> HistoryCoverage {
    if entries.is_empty() {
        return HistoryCoverage::EmptySnapshot;
    }
    let first = entries.first().expect("non-empty");
    let last = entries.last().expect("non-empty");
    let from = if before_more {
        HistoryBoundary::Position(HistoryBoundaryPosition {
            region: first.order.region.clone(),
            ordinal: first.order.ordinal,
            side: HistoryBoundarySide::Before,
        })
    } else {
        HistoryBoundary::Head
    };
    let to = if after_more {
        HistoryBoundary::Position(HistoryBoundaryPosition {
            region: last.order.region.clone(),
            ordinal: last.order.ordinal,
            side: HistoryBoundarySide::After,
        })
    } else {
        HistoryBoundary::Tail
    };
    HistoryCoverage::Interval(HistoryCoverageInterval { from, to })
}

fn map_resource_error(error: &SessionResourceError) -> HistoryBackendError {
    match error.kind() {
        SessionResourceErrorKind::NotFound => HistoryBackendError::NotFound,
        SessionResourceErrorKind::ReadOnlyStore => HistoryBackendError::PermissionDenied,
        SessionResourceErrorKind::Timeout => HistoryBackendError::Timeout,
        SessionResourceErrorKind::InvalidInput { .. } | SessionResourceErrorKind::Cursor(_) => {
            HistoryBackendError::InvalidCursor
        }
        SessionResourceErrorKind::HistoryWindow(error) => map_window_error(error),
        SessionResourceErrorKind::IndexNotReady { .. } => HistoryBackendError::IndexNotReady,
        SessionResourceErrorKind::Unsupported
        | SessionResourceErrorKind::Corrupt { .. }
        | SessionResourceErrorKind::Unavailable { .. }
        | SessionResourceErrorKind::Internal { .. }
        | SessionResourceErrorKind::Workspace(_)
        | SessionResourceErrorKind::PersistenceUncertain { .. }
        | SessionResourceErrorKind::SavedButNotAdmitted { .. }
        | SessionResourceErrorKind::Conflict { .. } => HistoryBackendError::BackendUnavailable,
    }
}

fn map_window_error(error: &HistoryWindowError) -> HistoryBackendError {
    match error {
        HistoryWindowError::SessionNotFound => HistoryBackendError::NotFound,
        HistoryWindowError::InvalidRequest { .. } => HistoryBackendError::InvalidCursor,
        HistoryWindowError::AnchorUnavailable { .. } => HistoryBackendError::AnchorUnavailable,
        HistoryWindowError::WindowInvalidated { .. }
        | HistoryWindowError::SnapshotMismatch { .. }
        | HistoryWindowError::CrossSession { .. }
        | HistoryWindowError::ReadTicketInvalid { .. }
        | HistoryWindowError::ReadTicketExpired => HistoryBackendError::SnapshotInvalidated,
        HistoryWindowError::SessionClosing | HistoryWindowError::MutationInProgress => {
            HistoryBackendError::Busy
        }
        HistoryWindowError::NotNegotiated | HistoryWindowError::ReadFailed { .. } => {
            HistoryBackendError::BackendUnavailable
        }
    }
}
