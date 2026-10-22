//! H5b：翻页/定位 reducer（设计 §7.2、§7.3、§8.4、§11.2、§11.3、§12.2）。
//!
//! 这里维护单一 V2 窗口的**页面状态**：保留页段、服务端游标、coverage 连续区间、
//! 显式 gap、阅读锚点、在途请求与保留预算。所有路径都是纯同步 reducer：
//!
//! - 请求只使用服务端返回的 cursor；`null` 表示该方向已证明耗尽，不再发请求。
//! - 迟到响应按 `requestKey` + `viewEpoch` + window/snapshot 身份校验；不匹配只丢弃，
//!   绝不写入新状态，也不自动重发旧请求。
//! - `seek` 是唯一改变导航代次的用户动作；它抢占旧 page 请求并清空旧的正文状态。
//! - payload 由有序 `segments` 重建；coverage 只在同一 region 且可比较时合并，空洞
//!   显式列入 `gaps`，不从 items 首尾推导连续性。
//! - 保留预算按条数/字节/页段数淘汰对侧整页；单页本身受协商预算约束。

use std::cmp::Ordering;
use std::collections::BTreeMap;

use peri_acp_types::history_v2::{
    DecimalU64, HISTORY_V2_VERSION, HistoryBoundary, HistoryBoundarySide, HistoryCoverage,
    HistoryCoverageInterval, HistoryDirection, HistoryEntry, HistoryEntryAnchor, HistoryLimits,
    HistoryWindowPage,
};

use super::HistoryV2Window;
use super::live::HistoryV2LiveState;
use super::request::{HistoryV2PageRequest, HistoryV2RequestContext, HistoryV2SeekRequest};
use super::validate::{PageValidationError, entry_identity_key, entry_key, validate_window_page};

/// 客户端窗口保留预算。单页本身由协商预算限制，这里防止连续翻页无限增长。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2Retention {
    pub max_items: usize,
    pub max_bytes: usize,
    pub max_segments: usize,
    pub max_body_bytes: usize,
}

impl Default for HistoryV2Retention {
    fn default() -> Self {
        Self {
            max_items: 200,
            max_bytes: 16 * 1024 * 1024,
            max_segments: 64,
            max_body_bytes: 4 * 1024 * 1024,
        }
    }
}

impl HistoryV2Retention {
    /// 首屏协商预算决定条数上界；其余预算使用设计建议值（§10.5）。
    pub fn for_limits(limits: &HistoryLimits) -> Self {
        Self {
            max_items: (limits.max_items as usize).saturating_mul(2).max(1),
            ..Self::default()
        }
    }
}

/// 首屏/seek 建立的“home”边界；翻页成功后仍指向用户当前所在页。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2Home {
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    pub anchor: Option<HistoryV2Anchor>,
}

/// 稳定条目/子项锚点 + 内容内偏移；不保存屏幕行号。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2Anchor {
    pub source: peri_acp_types::history_v2::HistorySource,
    pub entry_id: String,
    pub sub_item_id: Option<String>,
    pub content_offset: u64,
}

/// 锚点在当前保留窗口内是否仍可定位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2AnchorState {
    pub anchor: HistoryV2Anchor,
    pub retained: bool,
}

/// 一个服务端页段；页段是保留/淘汰的最小单位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2Segment {
    pub items: Vec<HistoryEntry>,
    /// `emptySnapshot` 页面为 None；其余页面必须携带区间。
    pub coverage: Option<HistoryCoverageInterval>,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    pub bytes: usize,
}

impl HistoryV2Segment {
    pub(crate) fn from_page(page: &HistoryWindowPage, items: Vec<HistoryEntry>) -> Self {
        let bytes = entries_bytes(&items);
        let coverage = match &page.coverage {
            HistoryCoverage::EmptySnapshot => None,
            HistoryCoverage::Interval(interval) => Some(interval.clone()),
        };
        Self {
            items,
            coverage,
            before_cursor: page.before_cursor.clone(),
            after_cursor: page.after_cursor.clone(),
            bytes,
        }
    }
}

/// 显式空洞：相邻两个 coverage 区间之间未物化的逻辑范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2Gap {
    pub from: HistoryBoundary,
    pub to: HistoryBoundary,
}

/// 页面状态的健康度；只有窗口级失效才阻断后续请求。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HistoryV2PagingStatus {
    #[default]
    Ready,
    Retryable {
        reason: String,
    },
    /// snapshot/window/ticket 失效：当前页仍可展示，但不能继续请求。
    ReopenRequired {
        reason: String,
    },
}

impl HistoryV2PagingStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn is_reopen_required(&self) -> bool {
        matches!(self, Self::ReopenRequired { .. })
    }
}

/// 在途 page/seek 请求。正文请求存放在各自的 body 状态中。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2InFlight {
    pub context: HistoryV2RequestContext,
    pub kind: HistoryV2InFlightKind,
    pub reserved_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2InFlightKind {
    Page {
        direction: HistoryDirection,
        cursor: String,
        limit: u32,
    },
    Seek {
        anchor: HistoryEntryAnchor,
        before: u32,
        after: u32,
    },
}

/// 窗口的页面状态；与窗口身份一起构成唯一的 V2 历史状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryV2Paging {
    pub view_epoch: u64,
    pub in_flight: Option<HistoryV2InFlight>,
    pub reserved_bytes: usize,
    pub home: Option<HistoryV2Home>,
    pub anchor: Option<HistoryV2AnchorState>,
    pub bodies: BTreeMap<String, super::body::HistoryV2BodyState>,
    pub retained_body_bytes: usize,
    pub live: HistoryV2LiveState,
    pub status: HistoryV2PagingStatus,
    pub last_error: Option<String>,
    /// 连接级单调 requestKey 的本地复核；初始页的 key 也计入。
    pub last_request_key: Option<DecimalU64>,
    pub retention: HistoryV2Retention,
}

impl HistoryV2Paging {
    fn reservation_for(&self, limits: &HistoryLimits) -> usize {
        usize::try_from(limits.target_page_bytes).unwrap_or(usize::MAX)
    }
}

/// 请求无法发起的原因；不包含 token/cursor 值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryV2RequestRejection {
    NotReady,
    ReopenRequired,
    Busy,
    Exhausted,
    InvalidLimit,
    InvalidAround,
    InvalidAnchor,
    ReusedRequestKey,
    BodyUnavailable,
    BodyComplete,
    BodyInFlight,
    InvalidBodyChunk,
    BudgetExceeded,
}

/// 响应被丢弃的原因。`InvalidPage` 只携带字段级描述。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2DiscardReason {
    NoInFlight,
    /// requestKey 不属于当前在途请求：迟到响应，不触碰新状态。
    LateRequest,
    EpochChanged,
    WrongWindow,
    WrongDirection,
    CursorMismatch,
    InvalidPage(String),
    DuplicateConflict,
    NoProgress,
}

/// 翻页结算结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2PageSettlement {
    Accepted {
        direction: HistoryDirection,
        added: usize,
        retained: usize,
    },
    Empty {
        direction: HistoryDirection,
    },
    Discarded(HistoryV2DiscardReason),
}

/// seek 结算结果；失败时旧页面保持可读。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2SeekSettlement {
    Accepted { retained: usize },
    AnchorUnavailable,
    Discarded(HistoryV2DiscardReason),
}

/// 取消/超时释放了哪一类在途标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryV2CancelOutcome {
    None,
    Page,
    Seek,
    Body,
}

impl HistoryV2Window {
    /// 沿窗口该方向的服务端 cursor 请求一页。
    ///
    /// 只允许一个 page/seek 在途；`beforeCursor/afterCursor = null` 时返回
    /// [`HistoryV2RequestRejection::Exhausted`]，不猜测上一页位置。
    pub fn request_page(
        &mut self,
        request_key: DecimalU64,
        direction: HistoryDirection,
        limit: u32,
        limits: &HistoryLimits,
    ) -> Result<HistoryV2PageRequest, HistoryV2RequestRejection> {
        self.ensure_paging_ready()?;
        if self.paging.in_flight.is_some() {
            return Err(HistoryV2RequestRejection::Busy);
        }
        if limit == 0 || limit > limits.max_items {
            return Err(HistoryV2RequestRejection::InvalidLimit);
        }
        self.ensure_request_key(request_key)?;
        let cursor = match direction {
            HistoryDirection::Before => self.before_cursor.clone(),
            HistoryDirection::After => self.after_cursor.clone(),
        }
        .ok_or(HistoryV2RequestRejection::Exhausted)?;

        let reserved_bytes = self.paging.reservation_for(limits);
        let context = self.base_context(request_key);
        let request = HistoryV2PageRequest {
            context: context.clone(),
            direction,
            cursor: cursor.clone(),
            limit,
            reserved_bytes,
        };
        self.paging.last_request_key = Some(request_key);
        self.paging.reserved_bytes = self.paging.reserved_bytes.saturating_add(reserved_bytes);
        self.paging.in_flight = Some(HistoryV2InFlight {
            context,
            kind: HistoryV2InFlightKind::Page {
                direction,
                cursor,
                limit,
            },
            reserved_bytes,
        });
        Ok(request)
    }

    /// 结算 `page` 响应。requestKey 必须先与当前在途请求匹配。
    pub fn settle_page(
        &mut self,
        request: &HistoryV2PageRequest,
        page: &HistoryWindowPage,
        limits: &HistoryLimits,
    ) -> HistoryV2PageSettlement {
        let Some(current) = self.paging.in_flight.clone() else {
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::NoInFlight);
        };
        if current.context.request_key != request.context.request_key {
            // 旧代次/旧请求的响应：保留当前在途请求，不释放它的预留。
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::LateRequest);
        }
        let in_flight = self
            .take_in_flight()
            .expect("checked in-flight request above");

        if request.context.view_epoch != self.paging.view_epoch {
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::EpochChanged);
        }
        let HistoryV2InFlightKind::Page {
            direction,
            cursor,
            limit,
        } = &in_flight.kind
        else {
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongDirection);
        };
        if *direction != request.direction || cursor != &request.cursor || *limit != request.limit {
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongDirection);
        }
        if request.context.window_id != self.window_id
            || request.context.session_id != self.session_id
            || request.context.snapshot_token != self.snapshot_token
        {
            self.note_retryable("history page request identity changed");
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongWindow);
        }
        if !self.page_matches_window(page) {
            self.note_retryable("history page window identity mismatch");
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongWindow);
        }
        if let Err(error) = validate_window_page(page, &self.session_id, limits) {
            let detail = page_validation_detail(error);
            self.note_retryable(detail.clone());
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::InvalidPage(detail));
        }
        if self.current_cursor(request.direction).as_deref() != Some(request.cursor.as_str()) {
            self.note_retryable("history page cursor no longer matches the retained window");
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::CursorMismatch);
        }
        if self.page_is_on_wrong_side(page, request.direction) {
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongDirection);
        }

        // 同一 identity 的不同 contentVersion 说明快照不一致，整页拒绝。
        let mut versions: BTreeMap<String, String> = self
            .items
            .iter()
            .map(|entry| (entry_key(entry), entry.content_version.clone()))
            .collect();
        for entry in &page.items {
            if let Some(existing) = versions.get(&entry_key(entry))
                && existing != &entry.content_version
            {
                self.note_retryable("history page changes an existing entry contentVersion");
                return HistoryV2PageSettlement::Discarded(
                    HistoryV2DiscardReason::DuplicateConflict,
                );
            }
        }
        let added_items: Vec<HistoryEntry> = page
            .items
            .iter()
            .filter(|entry| !versions.contains_key(&entry_key(entry)))
            .cloned()
            .collect();
        for entry in &added_items {
            versions.insert(entry_key(entry), entry.content_version.clone());
        }

        let cursor_advanced = self.cursor_advanced(page, request.direction, &request.cursor);
        if added_items.is_empty() && !cursor_advanced {
            self.note_retryable("history page made no progress");
            return HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::NoProgress);
        }

        let segment = HistoryV2Segment::from_page(page, added_items.clone());
        match request.direction {
            HistoryDirection::Before => self.segments.push_front(segment),
            HistoryDirection::After => self.segments.push_back(segment),
        }
        self.after_segments_changed(request.direction);
        self.progress = page.progress.clone();
        self.paging.status = HistoryV2PagingStatus::Ready;
        self.paging.last_error = None;

        if page.items.is_empty() {
            HistoryV2PageSettlement::Empty {
                direction: request.direction,
            }
        } else {
            HistoryV2PageSettlement::Accepted {
                direction: request.direction,
                added: added_items.len(),
                retained: self.items.len(),
            }
        }
    }

    /// 用户跳转：抢占旧的 page/seek 请求并推进导航代次，然后发出 seek。
    pub fn request_seek(
        &mut self,
        request_key: DecimalU64,
        anchor: &HistoryEntryAnchor,
        before: u32,
        after: u32,
        limits: &HistoryLimits,
    ) -> Result<HistoryV2SeekRequest, HistoryV2RequestRejection> {
        self.ensure_paging_ready()?;
        self.ensure_request_key(request_key)?;
        anchor
            .validate()
            .map_err(|_| HistoryV2RequestRejection::InvalidAnchor)?;
        let around = u64::from(before)
            .saturating_add(u64::from(after))
            .saturating_add(1);
        if around > u64::from(limits.max_items) {
            return Err(HistoryV2RequestRejection::InvalidAround);
        }

        // seek 改变导航代次：旧 page/body 响应此后一律按 key/epoch 丢弃。
        self.cancel_in_flight();
        self.cancel_all_bodies();
        self.paging.view_epoch = self.paging.view_epoch.saturating_add(1);
        let reserved_bytes = self.paging.reservation_for(limits);
        let context = self.base_context(request_key);
        let request = HistoryV2SeekRequest {
            context: context.clone(),
            anchor: anchor.clone(),
            before,
            after,
            reserved_bytes,
        };
        self.paging.last_request_key = Some(request_key);
        self.paging.reserved_bytes = self.paging.reserved_bytes.saturating_add(reserved_bytes);
        self.paging.in_flight = Some(HistoryV2InFlight {
            context,
            kind: HistoryV2InFlightKind::Seek {
                anchor: anchor.clone(),
                before,
                after,
            },
            reserved_bytes,
        });
        Ok(request)
    }

    /// 结算 seek。成功时用新页替换 payload 并更新锚点/home cursor。
    pub fn settle_seek(
        &mut self,
        request: &HistoryV2SeekRequest,
        page: &HistoryWindowPage,
        limits: &HistoryLimits,
    ) -> HistoryV2SeekSettlement {
        let Some(current) = self.paging.in_flight.clone() else {
            return HistoryV2SeekSettlement::Discarded(HistoryV2DiscardReason::NoInFlight);
        };
        if current.context.request_key != request.context.request_key {
            return HistoryV2SeekSettlement::Discarded(HistoryV2DiscardReason::LateRequest);
        }
        let in_flight = self
            .take_in_flight()
            .expect("checked in-flight request above");

        if request.context.view_epoch != self.paging.view_epoch {
            return HistoryV2SeekSettlement::Discarded(HistoryV2DiscardReason::EpochChanged);
        }
        let HistoryV2InFlightKind::Seek {
            anchor,
            before,
            after,
        } = &in_flight.kind
        else {
            return HistoryV2SeekSettlement::Discarded(HistoryV2DiscardReason::WrongDirection);
        };
        if anchor != &request.anchor || *before != request.before || *after != request.after {
            return HistoryV2SeekSettlement::Discarded(HistoryV2DiscardReason::WrongDirection);
        }
        if !self.page_matches_window(page) {
            self.note_retryable("history seek window identity mismatch");
            return HistoryV2SeekSettlement::Discarded(HistoryV2DiscardReason::WrongWindow);
        }
        if let Err(error) = validate_window_page(page, &self.session_id, limits) {
            let detail = page_validation_detail(error);
            self.note_retryable(detail.clone());
            return HistoryV2SeekSettlement::Discarded(HistoryV2DiscardReason::InvalidPage(detail));
        }
        let wanted = entry_identity_key(&request.anchor.source, &request.anchor.entry_id);
        let matches = page
            .items
            .iter()
            .filter(|entry| entry_key(entry) == wanted)
            .count();
        if matches != 1 {
            self.note_retryable("history seek anchor is not present exactly once");
            return HistoryV2SeekSettlement::AnchorUnavailable;
        }

        let segment = HistoryV2Segment::from_page(page, page.items.clone());
        self.segments.clear();
        self.segments.push_back(segment);
        self.before_cursor = page.before_cursor.clone();
        self.after_cursor = page.after_cursor.clone();
        self.rebuild_items();
        self.refresh_retained_counts();
        self.recompute_coverage();
        self.progress = page.progress.clone();
        let anchor = HistoryV2Anchor {
            source: request.anchor.source.clone(),
            entry_id: request.anchor.entry_id.clone(),
            sub_item_id: None,
            content_offset: 0,
        };
        self.paging.home = Some(HistoryV2Home {
            before_cursor: page.before_cursor.clone(),
            after_cursor: page.after_cursor.clone(),
            anchor: Some(anchor.clone()),
        });
        self.paging.anchor = Some(HistoryV2AnchorState {
            anchor,
            retained: true,
        });
        self.clear_bodies();
        self.paging.status = HistoryV2PagingStatus::Ready;
        self.paging.last_error = None;

        HistoryV2SeekSettlement::Accepted {
            retained: self.items.len(),
        }
    }

    /// 取消指定在途请求；返回实际释放的类别。
    pub fn cancel_request(&mut self, request_key: DecimalU64) -> HistoryV2CancelOutcome {
        if self
            .paging
            .in_flight
            .as_ref()
            .is_some_and(|current| current.context.request_key == request_key)
        {
            let kind = match self.paging.in_flight.as_ref().map(|current| &current.kind) {
                Some(HistoryV2InFlightKind::Seek { .. }) => HistoryV2CancelOutcome::Seek,
                _ => HistoryV2CancelOutcome::Page,
            };
            self.take_in_flight();
            return kind;
        }
        if self.cancel_body(request_key) {
            return HistoryV2CancelOutcome::Body;
        }
        HistoryV2CancelOutcome::None
    }

    /// 超时/传输失败：释放在途标记，保留当前 payload，允许用新 key 重试。
    pub fn fail_request(
        &mut self,
        request_key: DecimalU64,
        reason: impl Into<String>,
        reopen_required: bool,
    ) -> HistoryV2CancelOutcome {
        let reason = reason.into();
        let mut outcome = HistoryV2CancelOutcome::None;
        if self
            .paging
            .in_flight
            .as_ref()
            .is_some_and(|current| current.context.request_key == request_key)
        {
            let kind = match self.paging.in_flight.as_ref().map(|current| &current.kind) {
                Some(HistoryV2InFlightKind::Seek { .. }) => HistoryV2CancelOutcome::Seek,
                _ => HistoryV2CancelOutcome::Page,
            };
            self.take_in_flight();
            outcome = kind;
        }
        if self.fail_body(request_key, &reason) {
            outcome = HistoryV2CancelOutcome::Body;
        }
        if outcome != HistoryV2CancelOutcome::None {
            self.paging.status = if reopen_required {
                HistoryV2PagingStatus::ReopenRequired {
                    reason: reason.clone(),
                }
            } else {
                HistoryV2PagingStatus::Retryable {
                    reason: reason.clone(),
                }
            };
            self.paging.last_error = Some(reason);
        }
        outcome
    }

    /// 记录阅读锚点；锚点被淘汰后 `retained=false`，但身份继续保留。
    pub fn set_anchor(&mut self, anchor: HistoryV2Anchor) {
        let retained = self
            .items
            .iter()
            .any(|entry| entry_key(entry) == entry_identity_key(&anchor.source, &anchor.entry_id));
        self.paging.anchor = Some(HistoryV2AnchorState { anchor, retained });
    }

    pub(crate) fn base_context(&self, request_key: DecimalU64) -> HistoryV2RequestContext {
        HistoryV2RequestContext {
            request_key,
            session_id: self.session_id.clone(),
            window_id: self.window_id.clone(),
            snapshot_token: self.snapshot_token.clone(),
            read_ticket: self.read_ticket.clone(),
            view_epoch: self.paging.view_epoch,
        }
    }

    pub(crate) fn ensure_paging_ready(&self) -> Result<(), HistoryV2RequestRejection> {
        match &self.paging.status {
            HistoryV2PagingStatus::ReopenRequired { .. } => {
                Err(HistoryV2RequestRejection::ReopenRequired)
            }
            HistoryV2PagingStatus::Ready | HistoryV2PagingStatus::Retryable { .. } => Ok(()),
        }
    }

    pub(crate) fn ensure_request_key(
        &self,
        request_key: DecimalU64,
    ) -> Result<(), HistoryV2RequestRejection> {
        if self
            .paging
            .last_request_key
            .is_some_and(|last| request_key <= last)
        {
            return Err(HistoryV2RequestRejection::ReusedRequestKey);
        }
        Ok(())
    }

    fn current_cursor(&self, direction: HistoryDirection) -> Option<String> {
        match direction {
            HistoryDirection::Before => self.before_cursor.clone(),
            HistoryDirection::After => self.after_cursor.clone(),
        }
    }

    fn cursor_advanced(
        &self,
        page: &HistoryWindowPage,
        direction: HistoryDirection,
        requested: &str,
    ) -> bool {
        let next = match direction {
            HistoryDirection::Before => page.before_cursor.as_deref(),
            HistoryDirection::After => page.after_cursor.as_deref(),
        };
        match next {
            None => true,
            Some(next) => next != requested,
        }
    }

    fn page_matches_window(&self, page: &HistoryWindowPage) -> bool {
        page.version == HISTORY_V2_VERSION
            && page.session_id == self.session_id
            && page.window_id == self.window_id
            && page.snapshot_token == self.snapshot_token
            && page.read_ticket == self.read_ticket
            && page.history_revision == self.history_revision
            && page.committed_tail == self.committed_tail
            && page.projection_version == self.projection_version
    }

    /// 同 region 的 ordinal 比较；跨 region 无法证明方向，交回服务端游标约束。
    fn page_is_on_wrong_side(&self, page: &HistoryWindowPage, direction: HistoryDirection) -> bool {
        let (Some(edge), Some(existing)) = (
            match direction {
                HistoryDirection::Before => page.items.last(),
                HistoryDirection::After => page.items.first(),
            },
            match direction {
                HistoryDirection::Before => self.items.first(),
                HistoryDirection::After => self.items.last(),
            },
        ) else {
            return false;
        };
        if edge.order.region != existing.order.region {
            return false;
        }
        match direction {
            HistoryDirection::Before => edge.order.ordinal >= existing.order.ordinal,
            HistoryDirection::After => edge.order.ordinal <= existing.order.ordinal,
        }
    }

    fn take_in_flight(&mut self) -> Option<HistoryV2InFlight> {
        let in_flight = self.paging.in_flight.take()?;
        self.paging.reserved_bytes = self
            .paging
            .reserved_bytes
            .saturating_sub(in_flight.reserved_bytes);
        Some(in_flight)
    }

    fn cancel_in_flight(&mut self) {
        self.take_in_flight();
    }

    fn note_retryable(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.paging.last_error = Some(reason.clone());
        self.paging.status = HistoryV2PagingStatus::Retryable { reason };
    }

    fn after_segments_changed(&mut self, inserted: HistoryDirection) {
        self.rebuild_items();
        self.sync_edge_cursors();
        self.refresh_retained_counts();
        self.enforce_retention(inserted);
        self.rebuild_items();
        self.sync_edge_cursors();
        self.refresh_retained_counts();
        self.recompute_coverage();
        self.prune_bodies();
        self.refresh_anchor();
    }

    fn rebuild_items(&mut self) {
        self.items = self
            .segments
            .iter()
            .flat_map(|segment| segment.items.iter().cloned())
            .collect();
    }

    fn sync_edge_cursors(&mut self) {
        self.before_cursor = self
            .segments
            .front()
            .and_then(|segment| segment.before_cursor.clone());
        self.after_cursor = self
            .segments
            .back()
            .and_then(|segment| segment.after_cursor.clone());
    }

    fn refresh_retained_counts(&mut self) {
        self.retained_items = self
            .segments
            .iter()
            .map(|segment| segment.items.len())
            .sum();
        self.retained_bytes = self.segments.iter().map(|segment| segment.bytes).sum();
    }

    /// 淘汰对侧整页；单页是内存下界（单页已由协商预算限制）。
    fn enforce_retention(&mut self, inserted: HistoryDirection) {
        loop {
            let over = self.retained_items > self.paging.retention.max_items
                || self.retained_bytes > self.paging.retention.max_bytes
                || self.segments.len() > self.paging.retention.max_segments;
            if !over || self.segments.len() <= 1 {
                break;
            }
            match inserted {
                HistoryDirection::Before => {
                    self.segments.pop_back();
                }
                HistoryDirection::After => {
                    self.segments.pop_front();
                }
            }
            self.refresh_retained_counts();
        }
    }

    /// 从保留页段重建 coverage；只合并可证明相邻/重叠的区间。
    fn recompute_coverage(&mut self) {
        let mut merged: Vec<HistoryCoverageInterval> = Vec::new();
        for interval in self
            .segments
            .iter()
            .filter_map(|segment| segment.coverage.as_ref())
        {
            if let Some(last) = merged.last_mut()
                && let Some(order) = boundary_cmp(&last.to, &interval.from)
                && order != Ordering::Less
            {
                if boundary_cmp(&last.to, &interval.to) == Some(Ordering::Less) {
                    last.to = interval.to.clone();
                }
                continue;
            }
            merged.push(interval.clone());
        }
        self.gaps = coverage_gaps(&merged);
        self.empty_snapshot = !self.segments.is_empty()
            && self
                .segments
                .iter()
                .all(|segment| segment.coverage.is_none())
            && self.items.is_empty();
        self.coverage = merged;
    }

    fn refresh_anchor(&mut self) {
        let Some(state) = &mut self.paging.anchor else {
            return;
        };
        state.retained = self.items.iter().any(|entry| {
            entry_key(entry) == entry_identity_key(&state.anchor.source, &state.anchor.entry_id)
        });
    }
}

fn page_validation_detail(error: PageValidationError) -> String {
    match error {
        PageValidationError::SessionMismatch => "history page belongs to another session".into(),
        PageValidationError::Invalid(detail) => detail,
    }
}

fn entries_bytes(items: &[HistoryEntry]) -> usize {
    items
        .iter()
        .map(|entry| {
            serde_json::to_vec(entry)
                .map(|bytes| bytes.len())
                .unwrap_or(0)
        })
        .sum()
}

/// 边界比较：仅同 region 的 position 可比较；head < position < tail。
fn boundary_cmp(left: &HistoryBoundary, right: &HistoryBoundary) -> Option<Ordering> {
    match (left, right) {
        (HistoryBoundary::Head, HistoryBoundary::Head) => Some(Ordering::Equal),
        (HistoryBoundary::Tail, HistoryBoundary::Tail) => Some(Ordering::Equal),
        (HistoryBoundary::Head, _) => Some(Ordering::Less),
        (_, HistoryBoundary::Head) => Some(Ordering::Greater),
        (HistoryBoundary::Tail, _) => Some(Ordering::Greater),
        (_, HistoryBoundary::Tail) => Some(Ordering::Less),
        (HistoryBoundary::Position(left), HistoryBoundary::Position(right)) => {
            if left.region != right.region {
                return None;
            }
            Some(
                left.ordinal
                    .cmp(&right.ordinal)
                    .then_with(|| side_rank(left.side).cmp(&side_rank(right.side))),
            )
        }
    }
}

fn side_rank(side: HistoryBoundarySide) -> u8 {
    match side {
        HistoryBoundarySide::Before => 0,
        HistoryBoundarySide::After => 1,
    }
}

/// 显式 gap：同一 region 内两个覆盖区间之间的未物化范围。
fn coverage_gaps(intervals: &[HistoryCoverageInterval]) -> Vec<HistoryV2Gap> {
    let mut gaps = Vec::new();
    for pair in intervals.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        if boundary_cmp(&left.to, &right.from) == Some(Ordering::Less) {
            gaps.push(HistoryV2Gap {
                from: left.to.clone(),
                to: right.from.clone(),
            });
        }
    }
    gaps
}
