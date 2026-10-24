//! H5c：History V2 请求消费与触发（设计 §11.2、§11.3、§12.1）。
//!
//! V2 激活时页/seek/open 只走本模块：触发侧在同一个 reducer 写锁里认领请求并推进
//! `HISTORY_V2_STATE`，消费者只负责 RPC、严格解码与结算。V1 窗口队列不再接收 V2
//! 连接的任何请求，避免两套窗口状态混用。
//!
//! 迟到响应由 reducer 的 `requestKey + viewEpoch + window/snapshot` 校验丢弃；失败
//! 按 [`HistoryV2WireFailure`] 分类：窗口级失效保留旧页并要求重新 open，
//! `operation_in_progress` 保持原在途标记、不重复分配。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use peri_acp_types::history_v2::{
    DecimalU64, HISTORY_V2_VERSION, HistoryDirection, HistoryEntryAnchor, HistoryLimits,
    HistoryWindowPage,
};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::acp_client::AcpTuiClient;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms;

use super::error::HistoryV2WireFailure;
use super::outbound::{self, OutboundCommand, Submit, WindowEnqueue};
use super::paging::{HistoryV2PagingStatus, HistoryV2SeekSettlement};
use super::request::{
    HistoryV2PageRequest, HistoryV2RequestIdentity, HistoryV2SeekRequest, parse_page_result,
};
use super::{HistoryV2State, initial};

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// 当前协商预算；AtomStatic 只要求 Copy，预算按快照克隆。
fn current_limits() -> HistoryLimits {
    atoms::HISTORY_V2_LIMITS.state().read().clone()
}

/// 一次在途 open 的身份。用身份而不是裸 bool：旧会话的迟到失败不得清掉新会话的
/// 在途标记，也不得让新 open 误以为旧请求仍是自己的。
#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenClaim {
    session_id: String,
    request_key: DecimalU64,
    /// 认领时的连接代次；reset/会话边界后旧 Open 的迟到响应必须丢弃。
    epoch: u64,
}

static OPEN_IN_FLIGHT: Mutex<Option<OpenClaim>> = Mutex::new(None);
/// 消费者代次：reset（会话边界、切换、删除）时递增。Open 认领绑定该代次，
/// 避免 state 被清空后旧会话的成功响应重新建立窗口。
static CONSUMER_EPOCH: AtomicU64 = AtomicU64::new(0);

fn consumer_epoch() -> u64 {
    CONSUMER_EPOCH.load(Ordering::Relaxed)
}

impl HistoryV2RequestIdentity<'_> {
    /// 失败只允许作用于发出它的会话/窗口；open 没有 windowId，只校验会话。
    fn matches(&self, state: &HistoryV2State) -> bool {
        if state.session_id.as_deref() != Some(self.session_id) {
            return false;
        }
        let Some(window) = state.window.as_ref() else {
            return false;
        };
        if window.session_id != self.session_id {
            return false;
        }
        self.window_id
            .is_none_or(|window_id| window.window_id == window_id)
    }
}

/// 消费者队列里的请求；`Open` 只用于失败恢复后的重建与 V2 新会话首窗。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2ConsumerRequest {
    Open {
        session_id: String,
        request_key: DecimalU64,
        anchor: Option<HistoryEntryAnchor>,
    },
    Page(HistoryV2PageRequest),
    Seek(HistoryV2SeekRequest),
    /// 窗口拆除（best-effort）：只发送关闭事实，不修改本地状态。
    Close {
        session_id: String,
        request_key: DecimalU64,
        window_id: String,
        snapshot_token: String,
        read_ticket: String,
    },
}

impl HistoryV2ConsumerRequest {
    pub(crate) fn session_id(&self) -> &str {
        match self {
            Self::Open { session_id, .. } => session_id,
            Self::Close { session_id, .. } => session_id,
            Self::Page(request) => &request.context.session_id,
            Self::Seek(request) => &request.context.session_id,
        }
    }

    pub(crate) fn request_key(&self) -> DecimalU64 {
        match self {
            Self::Open { request_key, .. } => *request_key,
            Self::Close { request_key, .. } => *request_key,
            Self::Page(request) => request.context.request_key,
            Self::Seek(request) => request.context.request_key,
        }
    }

    /// 失败结算身份：session + window（open/close 为 None）+ requestKey。
    fn identity(&self) -> HistoryV2RequestIdentity<'_> {
        match self {
            Self::Open { .. } | Self::Close { .. } => HistoryV2RequestIdentity {
                session_id: self.session_id(),
                window_id: None,
                request_key: self.request_key(),
            },
            Self::Page(request) => request.context.identity(),
            Self::Seek(request) => request.context.identity(),
        }
    }

    /// `_peri/history` 的完整 params。open 不带 window/ticket，其余带。
    pub fn params(&self, limits: &HistoryLimits) -> Value {
        match self {
            Self::Open {
                session_id,
                request_key,
                anchor,
            } => {
                let anchor = match anchor {
                    None => json!({"kind": "latest"}),
                    Some(anchor) => json!({
                        "kind": "entry",
                        "entryId": anchor.entry_id,
                        "source": source_value(&anchor.source),
                    }),
                };
                json!({
                    "version": HISTORY_V2_VERSION,
                    "action": "open",
                    "sessionId": session_id,
                    "requestKey": request_key,
                    "anchor": anchor,
                    "before": limits.default_items,
                    "after": 0,
                    "targetBytes": limits.target_page_bytes,
                })
            }
            Self::Close {
                session_id,
                request_key,
                window_id,
                snapshot_token,
                read_ticket,
            } => json!({
                "version": HISTORY_V2_VERSION,
                "action": "close",
                "sessionId": session_id,
                "requestKey": request_key,
                "windowId": window_id,
                "snapshotToken": snapshot_token,
                "readTicket": read_ticket,
            }),
            Self::Page(request) => request.params(),
            Self::Seek(request) => request.params(),
        }
    }
}

fn source_value(source: &peri_acp_types::history_v2::HistorySource) -> Value {
    match source {
        peri_acp_types::history_v2::HistorySource::Own => json!({"kind": "own"}),
        peri_acp_types::history_v2::HistorySource::Inherited(origin) => json!({
            "kind": "inherited",
            "snapshotId": origin.snapshot_id,
            "ancestorSessionId": origin.ancestor_session_id,
        }),
    }
}

/// 本地 beta 与连接协商同时为真；V1 窗口路径据此让位。
pub(crate) fn v2_path_active() -> bool {
    atoms::HISTORY_V2_CAPABLE.get()
}

/// 下一个连接级 requestKey。initial 与 open/page/seek/body 共用同一分配器
/// （设计 §3.2：requestKey 是“当前连接全局严格递增”的操作号）。
pub(crate) fn seed_request_key(key: DecimalU64) {
    atoms::HISTORY_V2_NEXT_KEY.fetch_max(key.get(), Ordering::Relaxed);
}

pub(crate) fn next_request_key() -> DecimalU64 {
    DecimalU64::new(atoms::HISTORY_V2_NEXT_KEY.fetch_add(1, Ordering::Relaxed) + 1)
}

#[cfg(test)]
pub(crate) fn reset_request_key_for_test() {
    atoms::HISTORY_V2_NEXT_KEY.store(0, Ordering::Relaxed);
}

/// 一个已交付窗口的关闭身份。关闭只依赖服务端事实，不读回当前状态，避免在持有
/// 状态锁时再次进入入队临界区。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryV2WindowIdentity {
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
}

impl HistoryV2WindowIdentity {
    pub(crate) fn from_state(state: &HistoryV2State) -> Option<Self> {
        state.window.as_ref().map(Self::from_window)
    }

    fn from_window(window: &super::HistoryV2Window) -> Self {
        Self {
            session_id: window.session_id.clone(),
            window_id: window.window_id.clone(),
            snapshot_token: window.snapshot_token.clone(),
            read_ticket: window.read_ticket.clone(),
        }
    }
}

/// H5c：详情面板的正文请求。详情是窗口内容的派生阅读器：按 §11.4 使用独立的
/// 详情预算和随机 offset 语义，不写窗口页/正文账目，也不参与窗口代次结算；
/// 响应由 `HistoryV2BodyBinding` 的 window/snapshot/bodyRef 校验。requestKey 与
/// 页/seek/open/initial 共用同一分配器，并且与入队在同一临界区内完成。
pub(crate) fn submit_body(
    binding: &crate::kit::history_detail_binding::HistoryV2BodyBinding,
    offset: u64,
    max_bytes: u32,
) -> Result<Submit<super::request::HistoryV2BodyRequest>, outbound::SubmitError> {
    outbound::submit_body(|request_key| super::request::HistoryV2BodyRequest {
        context: super::request::HistoryV2RequestContext {
            request_key,
            session_id: binding.session_id.clone(),
            window_id: binding.window_id.clone(),
            snapshot_token: binding.snapshot_token.clone(),
            read_ticket: binding.read_ticket.clone(),
            view_epoch: 0,
        },
        body_key: String::new(),
        body_ref: binding.body_ref.clone(),
        offset: DecimalU64::new(offset),
        max_bytes,
    })
}

/// 桥接 reset 只清理视图投影；在途认领由生命周期同步动作（new/load/delete）
/// 清理，避免桥接任务的异步观察晚于新会话的 Open 入队。
pub(crate) fn reset_view_projection() {
    atoms::HISTORY_V2_PREFIX_LEN.set(0);
    atoms::HISTORY_V2_VIEW_EPOCH.set(0);
}

/// 会话边界复位：作废在途 open 并重置视图投影。
pub(crate) fn reset_consumer_state() {
    *open_claim_slot() = None;
    CONSUMER_EPOCH.fetch_add(1, Ordering::Relaxed);
    reset_view_projection();
}

fn open_claim_slot() -> std::sync::MutexGuard<'static, Option<OpenClaim>> {
    OPEN_IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn try_claim_open(session_id: &str, request_key: DecimalU64) -> Option<OpenClaim> {
    let mut slot = open_claim_slot();
    if slot.is_some() {
        return None;
    }
    let claim = OpenClaim {
        session_id: session_id.to_owned(),
        request_key,
        epoch: consumer_epoch(),
    };
    *slot = Some(claim.clone());
    Some(claim)
}

/// 取回与本次响应完全匹配的认领；reset 后槽位已空，旧响应返回 `None`。
fn take_open_claim(session_id: &str, request_key: DecimalU64) -> Option<OpenClaim> {
    let mut slot = open_claim_slot();
    if slot
        .as_ref()
        .is_some_and(|claim| claim.session_id == session_id && claim.request_key == request_key)
    {
        slot.take()
    } else {
        None
    }
}

fn release_open_for(session_id: &str, request_key: DecimalU64) {
    let mut slot = open_claim_slot();
    if slot
        .as_ref()
        .is_some_and(|claim| claim.session_id == session_id && claim.request_key == request_key)
    {
        *slot = None;
    }
}

#[cfg(test)]
fn open_claim_is_some() -> bool {
    open_claim_slot().is_some()
}

pub(crate) fn notify_view(state: &HistoryV2State) {
    #[cfg(test)]
    NOTIFY_COUNT.fetch_add(1, Ordering::Relaxed);
    let Some(tx) = atoms::LOCAL_EVENT_TX.get() else {
        return;
    };
    let _ = tx.send(AcpEventWithEpoch {
        event: AcpEventData::HistoryV2ViewChanged {
            state: Box::new(state.clone()),
        },
        active_session_id: state.session_id.clone().unwrap_or_default(),
    });
}

/// 测试用：`notify_view` 的调用次数（即使没有安装 LOCAL_EVENT_TX 也计数）。
#[cfg(test)]
static NOTIFY_COUNT: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub(crate) fn notify_count() -> u64 {
    NOTIFY_COUNT.load(Ordering::Relaxed)
}
#[cfg(test)]
fn publish_state(state: HistoryV2State) {
    atoms::HISTORY_V2_STATE.set(state.clone());
    notify_view(&state);
}

/// 只读判断：窗口级失效时滚动触发先尝试重新 open。
fn reopen_required() -> bool {
    let handle = atoms::HISTORY_V2_STATE.state();
    let state = handle.read();
    state
        .window
        .as_ref()
        .is_some_and(|window| window.paging.status.is_reopen_required())
}

pub(crate) fn request_older_page_if_due() -> bool {
    request_page_if_due(HistoryDirection::Before)
}

pub(crate) fn request_newer_page_if_due() -> bool {
    request_page_if_due(HistoryDirection::After)
}

fn request_page_if_due(direction: HistoryDirection) -> bool {
    if !v2_path_active() {
        return false;
    }
    if reopen_required() {
        return request_reopen(false);
    }
    let limits = current_limits();
    match enqueue_page(direction, limits) {
        WindowEnqueue::Sent(_) => true,
        WindowEnqueue::Closed(request_key) => {
            rollback_enqueued_request(request_key);
            false
        }
        WindowEnqueue::Rejected | WindowEnqueue::Unavailable => false,
    }
}

/// 在同一个入队临界区内分配 key 并登记 reducer 在途状态；key 与 wire 完全一致。
fn enqueue_page(direction: HistoryDirection, limits: HistoryLimits) -> WindowEnqueue {
    outbound::enqueue_window(|request_key| {
        let handle = atoms::HISTORY_V2_STATE.state();
        let mut state = handle.write();
        if !state.is_ready() {
            return None;
        }
        let window = state.window.as_mut()?;
        match window.request_page(request_key, direction, limits.default_items, &limits) {
            Ok(request) => Some(HistoryV2ConsumerRequest::Page(request)),
            Err(rejection) => {
                debug!(?direction, ?rejection, "history_v2: page trigger rejected");
                None
            }
        }
    })
}

/// 定位锚点后发出 seek；锚点身份来自当前保留窗口的稳定条目。
pub(crate) fn request_seek_if_due(anchor: HistoryEntryAnchor) -> bool {
    if !v2_path_active() {
        return false;
    }
    let limits = current_limits();
    match enqueue_seek(anchor, limits) {
        WindowEnqueue::Sent(_) => true,
        WindowEnqueue::Closed(request_key) => {
            rollback_enqueued_request(request_key);
            false
        }
        WindowEnqueue::Rejected | WindowEnqueue::Unavailable => false,
    }
}

fn enqueue_seek(anchor: HistoryEntryAnchor, limits: HistoryLimits) -> WindowEnqueue {
    outbound::enqueue_window(|request_key| {
        let handle = atoms::HISTORY_V2_STATE.state();
        let mut state = handle.write();
        if !state.is_ready() {
            return None;
        }
        let window = state.window.as_mut()?;
        let before = limits.default_items / 2;
        let after = limits.default_items.saturating_sub(before);
        match window.request_seek(request_key, &anchor, before, after, &limits) {
            Ok(request) => Some(HistoryV2ConsumerRequest::Seek(request)),
            Err(rejection) => {
                debug!(?rejection, "history_v2: seek trigger rejected");
                None
            }
        }
    })
}

/// 队列已关闭时的本地回滚：请求从未上线，释放刚登记的在途/预留并重新投影。
fn rollback_enqueued_request(request_key: DecimalU64) {
    {
        let handle = atoms::HISTORY_V2_STATE.state();
        let mut state = handle.write();
        if let Some(window) = state.window.as_mut() {
            window.cancel_request(request_key);
        }
    }
    notify_current_state();
}

/// 缺口恢复：用保留窗口里离缺口最近的稳定条目 seek 回缺口。
pub(crate) fn request_gap_if_due() -> bool {
    if !v2_path_active() {
        return false;
    }
    let Some(anchor) = gap_anchor() else {
        return false;
    };
    request_seek_if_due(anchor)
}

fn gap_anchor() -> Option<HistoryEntryAnchor> {
    use peri_acp_types::history_v2::{HistoryBoundary, HistoryBoundarySide};
    let handle = atoms::HISTORY_V2_STATE.state();
    let state = handle.read();
    let window = state.window.as_ref()?;
    let gap = window.gaps.first()?;
    let (region, ordinal, prefer_after) = match &gap.to {
        HistoryBoundary::Position(position) => (
            &position.region,
            position.ordinal.get(),
            position.side == HistoryBoundarySide::After,
        ),
        _ => return None,
    };
    let entry = window
        .items
        .iter()
        .filter(|entry| &entry.order.region == region)
        .filter(|entry| {
            let value = entry.order.ordinal.get();
            if prefer_after {
                value >= ordinal
            } else {
                value <= ordinal
            }
        })
        .min_by_key(|entry| entry.order.ordinal.get().abs_diff(ordinal))?;
    Some(HistoryEntryAnchor {
        entry_id: entry.entry_id.clone(),
        source: entry.source.clone(),
    })
}

/// 重新 open 当前会话；`force=false` 只在 reducer 要求重开时动作。
///
/// 替换顺序是 Open → 接纳 → Close：Open 失败时旧窗口保持可读，不发送 Close。
pub(crate) fn request_reopen(force: bool) -> bool {
    if !v2_path_active() {
        return false;
    }
    if !force && !reopen_required() {
        return false;
    }
    let (session_id, anchor) = {
        let handle = atoms::HISTORY_V2_STATE.state();
        let state = handle.read();
        let Some(window) = state.window.as_ref().filter(|_| state.is_ready()) else {
            return false;
        };
        let anchor = window
            .paging
            .anchor
            .as_ref()
            .filter(|state| state.retained)
            .map(|state| HistoryEntryAnchor {
                entry_id: state.anchor.entry_id.clone(),
                source: state.anchor.source.clone(),
            });
        (window.session_id.clone(), anchor)
    };
    begin_open(session_id, anchor)
}

/// 新会话首个 V2 窗口：等价于失败恢复用的 open，但允许在空状态上建立窗口。
///
/// 不伪造 `session/load` initial；服务端 `open` 返回的页面就是首窗。
pub(crate) fn request_open_latest(session_id: &str) -> bool {
    if !v2_path_active() || session_id.is_empty() {
        return false;
    }
    begin_open(session_id.to_owned(), None)
}

/// 分配 Open 的 requestKey 并入队；claim 与入队共用同一个临界区。
fn begin_open(session_id: String, anchor: Option<HistoryEntryAnchor>) -> bool {
    match outbound::enqueue_window(|request_key| {
        try_claim_open(&session_id, request_key)?;
        Some(HistoryV2ConsumerRequest::Open {
            session_id: session_id.clone(),
            request_key,
            anchor,
        })
    }) {
        WindowEnqueue::Sent(_) => true,
        // 已分配 key 但队列关闭：回滚 claim，避免永久占用在途标记。
        WindowEnqueue::Closed(request_key) => {
            release_open_for(&session_id, request_key);
            false
        }
        WindowEnqueue::Rejected | WindowEnqueue::Unavailable => false,
    }
}

/// 关闭当前 V2 窗口（best-effort）。
///
/// 只把关闭请求放进队列，**不**在本地标记窗口已关闭：真正的关闭确认属于服务端。
pub(crate) fn request_close_current() -> bool {
    enqueue_close(None)
}

/// 关闭指定会话的当前 V2 窗口；会话不匹配时不动其他会话的窗口。
pub(crate) fn request_close_for_session(session_id: &str) -> bool {
    enqueue_close(Some(session_id))
}

/// 关闭一个已捕获的旧窗口身份；Open/initial 成功接纳后才调用。
pub(crate) fn request_close_window(identity: HistoryV2WindowIdentity) -> bool {
    matches!(enqueue_close_identity(identity), WindowEnqueue::Sent(_))
}

fn enqueue_close(expected_session: Option<&str>) -> bool {
    let identity = {
        let handle = atoms::HISTORY_V2_STATE.state();
        let state = handle.read();
        if !state.is_ready() {
            return false;
        }
        let Some(session_id) = state.session_id.as_deref() else {
            return false;
        };
        if expected_session.is_some_and(|expected| expected != session_id) {
            return false;
        }
        let Some(window) = state.window.as_ref() else {
            return false;
        };
        HistoryV2WindowIdentity::from_window(window)
    };
    request_close_window(identity)
}

fn enqueue_close_identity(identity: HistoryV2WindowIdentity) -> WindowEnqueue {
    outbound::enqueue_window(|request_key| {
        Some(HistoryV2ConsumerRequest::Close {
            session_id: identity.session_id,
            request_key,
            window_id: identity.window_id,
            snapshot_token: identity.snapshot_token,
            read_ticket: identity.read_ticket,
        })
    })
}

/// 结算一页的核心：只修改调用方持有的状态，不发布（发布需要原子写锁之外的
/// 上下文；在写锁内调用 `Atom::set` 会自我阻塞）。
fn apply_page(
    state: &mut HistoryV2State,
    request: &HistoryV2PageRequest,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> bool {
    state.window.as_mut().is_some_and(|window| {
        matches!(
            window.settle_page(request, page, limits),
            super::paging::HistoryV2PageSettlement::Accepted { .. }
                | super::paging::HistoryV2PageSettlement::Empty { .. }
        )
    })
}

/// 测试/同步调用入口：修改后发布。
#[cfg(test)]
pub(crate) fn settle_page(
    state: &mut HistoryV2State,
    request: &HistoryV2PageRequest,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> bool {
    let accepted = apply_page(state, request, page, limits);
    if accepted {
        publish_state(state.clone());
    }
    accepted
}

fn apply_seek(
    state: &mut HistoryV2State,
    request: &HistoryV2SeekRequest,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> bool {
    state.window.as_mut().is_some_and(|window| {
        matches!(
            window.settle_seek(request, page, limits),
            HistoryV2SeekSettlement::Accepted { .. }
        )
    })
}

#[cfg(test)]
pub(crate) fn settle_seek(
    state: &mut HistoryV2State,
    request: &HistoryV2SeekRequest,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> bool {
    let accepted = apply_seek(state, request, page, limits);
    if accepted {
        publish_state(state.clone());
    }
    accepted
}

/// 失败结算核心：返回 `None` 表示迟到/不适用；`Some(retryable)` 表示已修改状态。
fn apply_failure_core(
    state: &mut HistoryV2State,
    identity: HistoryV2RequestIdentity<'_>,
    failure: &HistoryV2WireFailure,
) -> Option<bool> {
    if !identity.matches(state) {
        debug!(
            session_id = identity.session_id,
            "history_v2: ignoring a failure from another session/window"
        );
        return None;
    }
    let Some(window) = state.window.as_mut() else {
        return None;
    };
    // page/seek 的失败只有仍对应当前在途请求时才允许结算；被替代请求的迟到失败
    // 不得释放/覆盖新的在途认领。open 没有 windowId，不参与这条 page 认领比较。
    if identity.window_id.is_some()
        && window
            .paging
            .in_flight
            .as_ref()
            .is_some_and(|current| current.context.request_key != identity.request_key)
    {
        debug!("history_v2: ignoring a failure for a superseded request");
        return None;
    }
    if failure.operation_in_progress() {
        // 原操作仍在服务端执行：保留在途标记，触发侧不会重复分配。
        window.paging.last_error = Some(failure.label().to_owned());
        return Some(false);
    }
    let reopen = failure.requires_reopen();
    // Open 没有 windowId；它的任何域失败都必须回到 reopen，而不是普通重试。
    let is_open_request = identity.window_id.is_none();
    let discard = failure.discard_only() || failure.negotiation_failure();
    // 只有当前在途认领属于本次请求时才释放它；open 失败没有 page 认领，直接记录状态。
    let claimed = window
        .paging
        .in_flight
        .as_ref()
        .is_some_and(|current| current.context.request_key == identity.request_key);
    if discard && claimed {
        window.cancel_request(identity.request_key);
    } else if claimed {
        window.fail_request(identity.request_key, failure.label(), reopen);
    } else {
        window.paging.last_error = Some(failure.label().to_owned());
        window.paging.status = if reopen || is_open_request {
            HistoryV2PagingStatus::ReopenRequired {
                reason: failure.label().to_owned(),
            }
        } else {
            HistoryV2PagingStatus::Retryable {
                reason: failure.label().to_owned(),
            }
        };
    }
    Some(!reopen)
}

/// 失败结算。返回是否允许后续用新 requestKey 重试。
///
/// 迟到失败只允许作用于发出它的 session/window/requestKey；任一不匹配都整体忽略，
/// 不释放在途标记、不改窗口状态。
#[cfg(test)]
pub(crate) fn apply_failure(
    state: &mut HistoryV2State,
    identity: HistoryV2RequestIdentity<'_>,
    failure: &HistoryV2WireFailure,
) -> bool {
    match apply_failure_core(state, identity, failure) {
        None => false,
        Some(retryable) => {
            publish_state(state.clone());
            retryable
        }
    }
}

/// 传输失败核心：返回是否修改了状态。
fn apply_transport_failure_core(
    state: &mut HistoryV2State,
    identity: HistoryV2RequestIdentity<'_>,
    label: &str,
) -> bool {
    if !identity.matches(state) {
        return false;
    }
    if let Some(window) = state.window.as_mut() {
        window.fail_request(identity.request_key, label, false);
    }
    true
}

/// 传输失败/超时：释放当前在途并允许重试；跨会话/窗口的迟到失败直接忽略。
#[cfg(test)]
pub(crate) fn apply_transport_failure(
    state: &mut HistoryV2State,
    identity: HistoryV2RequestIdentity<'_>,
    label: &str,
) {
    if apply_transport_failure_core(state, identity, label) {
        publish_state(state.clone());
    }
}

/// open 失败的核心：归还 claim 并修改同会话窗口；返回是否需要发布。
fn apply_open_failure(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    label: &str,
) -> bool {
    release_open_for(session_id, request_key);
    if state
        .session_id
        .as_deref()
        .is_some_and(|current| current != session_id)
    {
        debug!(
            session_id,
            "history_v2: ignoring a late open failure for another session"
        );
        return false;
    }
    if let Some(window) = state.window.as_mut() {
        window.paging.status = HistoryV2PagingStatus::ReopenRequired {
            reason: label.to_owned(),
        };
        window.paging.last_error = Some(label.to_owned());
    }
    true
}

#[cfg(test)]
fn settle_open_failure(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    label: &str,
) {
    if apply_open_failure(state, session_id, request_key, label) {
        publish_state(state.clone());
    }
}

enum OpenSettlement {
    /// 迟到/不属于当前会话：不改状态。
    Unchanged,
    /// 已修改状态；成功接纳时携带待关闭的旧窗口身份。
    Changed {
        close: Option<HistoryV2WindowIdentity>,
    },
}

fn apply_open(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    claim_epoch: Option<u64>,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> OpenSettlement {
    // reset/会话边界之后，旧认领的迟到响应不得在空状态上重新建立窗口。
    if claim_epoch.is_some_and(|epoch| epoch != consumer_epoch()) {
        debug!(
            session_id,
            "history_v2: ignoring an open response from a reset consumer epoch"
        );
        return OpenSettlement::Unchanged;
    }
    // 旧会话的 open 响应不能在会话切换后改写新会话的窗口。
    if state
        .session_id
        .as_deref()
        .is_some_and(|current| current != session_id)
    {
        debug!(
            session_id,
            "history_v2: ignoring a late open response for another session"
        );
        return OpenSettlement::Unchanged;
    }
    let previous = state.clone();
    // initial 发布后，比当前高水位更旧的 Open 是排队期间被替换的候选：不得用它
    // 覆盖已经提交的窗口（同会话重复 load 的典型迟到响应）。
    if previous
        .window
        .as_ref()
        .and_then(|window| window.paging.last_request_key)
        .is_some_and(|current| request_key.get() <= current.get())
    {
        debug!(
            request_key = request_key.get(),
            "history_v2: ignoring a stale open response"
        );
        return OpenSettlement::Unchanged;
    }
    // 空状态 = `session/new` 的首窗；已有状态 = 失效后的重建。
    let admitted = if previous.session_id.is_none() && previous.window.is_none() {
        initial::admit_new_session_open(session_id, page, limits, request_key)
    } else {
        initial::admit_open_response(&previous, session_id, page, limits, request_key)
    };
    match admitted {
        Ok(next) => {
            let previous_window = previous
                .window
                .as_ref()
                .map(HistoryV2WindowIdentity::from_window);
            let next_window = next
                .window
                .as_ref()
                .map(HistoryV2WindowIdentity::from_window);
            *state = next;
            OpenSettlement::Changed {
                // 只有新窗口被接纳后才释放旧身份；相同身份不重复关闭。
                close: previous_window.filter(|old| next_window.as_ref() != Some(old)),
            }
        }
        Err(error) => {
            if let Some(window) = state.window.as_mut() {
                window.paging.status = HistoryV2PagingStatus::ReopenRequired {
                    reason: error.to_string(),
                };
                window.paging.last_error = Some(error.to_string());
            }
            warn!(reason = %error, "history_v2: reopen response rejected");
            OpenSettlement::Changed { close: None }
        }
    }
}

/// 测试/同步调用入口：修改后发布，返回接纳成功后的旧窗口身份。
#[cfg(test)]
fn settle_open(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> Option<HistoryV2WindowIdentity> {
    match apply_open(state, session_id, request_key, None, page, limits) {
        OpenSettlement::Unchanged => None,
        OpenSettlement::Changed { close } => {
            publish_state(state.clone());
            close
        }
    }
}

/// 启动 V2 消费任务：按入队顺序串行发送所有 V2 RPC。
///
/// 窗口请求在这里结算；body / session-load 只回传原始响应，由其调用方继续做
/// 严格解码与生命周期提交。
pub(crate) fn spawn_history_v2_consumer(
    client: AcpTuiClient,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<OutboundCommand>,
    shutdown: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let command = tokio::select! {
                _ = shutdown.cancelled() => break,
                command = rx.recv() => match command {
                    Some(command) => command,
                    None => break,
                },
            };
            match command {
                OutboundCommand::Load { params, reply } => {
                    // 生命周期仍由 session.rs 负责；调度器只保证发送顺序。
                    let result = client.send_session_load(params).await;
                    let _ = reply.send(result);
                }
                OutboundCommand::Body { params, reply } => {
                    let result =
                        match peri_time::timeout(FETCH_TIMEOUT, client.history_v2_request(params))
                            .await
                        {
                            Ok(result) => result,
                            Err(_) => Err(peri_acp::transport::types::AcpError::new(
                                -32603,
                                "history body request timed out",
                            )),
                        };
                    let _ = reply.send(result);
                }
                OutboundCommand::Request(request) => {
                    let close_target = handle_window_request(&client, request).await;
                    if let Some(identity) = close_target {
                        // Open 已被接纳：此时才释放旧窗口身份。
                        let _ = request_close_window(identity);
                    }
                }
            }
        }
    })
}

/// 发送并结算一个窗口请求；成功接纳新窗口时返回待关闭的旧窗口身份。
async fn handle_window_request(
    client: &AcpTuiClient,
    request: HistoryV2ConsumerRequest,
) -> Option<HistoryV2WindowIdentity> {
    let limits = current_limits();
    let identity = request.identity();
    let params = request.params(&limits);
    let response = peri_time::timeout(FETCH_TIMEOUT, client.history_v2_request(params)).await;
    if matches!(&request, HistoryV2ConsumerRequest::Close { .. }) {
        // best-effort：关闭失败也不回滚/伪造本地状态；服务端会话拆除仍会清理。
        match &response {
            Ok(Ok(_)) => debug!("history_v2: window close acknowledged"),
            Ok(Err(error)) => {
                debug!(%error, "history_v2: window close rejected")
            }
            Err(_) => debug!("history_v2: window close timed out"),
        }
        return None;
    }
    match response {
        Ok(Ok(raw)) => {
            let page = match parse_page_result(&raw) {
                Ok(page) => page,
                Err(_) => {
                    let notify = {
                        let handle = atoms::HISTORY_V2_STATE.state();
                        let mut state = handle.write();
                        if matches!(&request, HistoryV2ConsumerRequest::Open { .. }) {
                            apply_open_failure(
                                &mut state,
                                identity.session_id,
                                identity.request_key,
                                "invalid_page_shape",
                            )
                        } else {
                            apply_transport_failure_core(&mut state, identity, "invalid_page_shape")
                        }
                    };
                    if notify {
                        notify_current_state();
                    }
                    return None;
                }
            };
            // 原子写锁内只修改状态；发布（notify_view）必须等锁释放后，否则
            // `Atom::set` 会再次申请同一把写锁而自我阻塞。
            let (close_target, notify) = {
                let handle = atoms::HISTORY_V2_STATE.state();
                let mut state = handle.write();
                match &request {
                    HistoryV2ConsumerRequest::Open {
                        session_id,
                        request_key,
                        ..
                    } => {
                        // 认领必须仍然存在且属于目标会话；reset/切换后的迟到
                        // 成功响应不得在空状态上建立旧窗口。
                        let Some(claim) = take_open_claim(session_id, *request_key) else {
                            debug!(
                                session_id,
                                "history_v2: ignoring an open response without a live claim"
                            );
                            return None;
                        };
                        match apply_open(
                            &mut state,
                            session_id,
                            *request_key,
                            Some(claim.epoch),
                            &page,
                            &limits,
                        ) {
                            OpenSettlement::Unchanged => (None, false),
                            OpenSettlement::Changed { close } => (close, true),
                        }
                    }
                    HistoryV2ConsumerRequest::Page(page_request) => {
                        (None, apply_page(&mut state, page_request, &page, &limits))
                    }
                    HistoryV2ConsumerRequest::Seek(seek_request) => {
                        (None, apply_seek(&mut state, seek_request, &page, &limits))
                    }
                    // Close 已在解析页之前处理并返回。
                    HistoryV2ConsumerRequest::Close { .. } => (None, false),
                }
            };
            if notify {
                notify_current_state();
            }
            close_target
        }
        Ok(Err(error)) => {
            let failure = HistoryV2WireFailure::from_error(&error);
            debug!(
                reason = failure.label(),
                "history_v2: request failed with a domain error"
            );
            if matches!(&request, HistoryV2ConsumerRequest::Open { .. }) {
                release_open_for(identity.session_id, identity.request_key);
            }
            let notify = {
                let handle = atoms::HISTORY_V2_STATE.state();
                let mut state = handle.write();
                apply_failure_core(&mut state, identity, &failure).is_some()
            };
            if notify {
                notify_current_state();
            }
            None
        }
        Err(_) => {
            let notify = {
                let handle = atoms::HISTORY_V2_STATE.state();
                let mut state = handle.write();
                if matches!(&request, HistoryV2ConsumerRequest::Open { .. }) {
                    apply_open_failure(
                        &mut state,
                        identity.session_id,
                        identity.request_key,
                        "timeout",
                    )
                } else {
                    apply_transport_failure_core(&mut state, identity, "timeout")
                }
            };
            if notify {
                notify_current_state();
            }
            None
        }
    }
}

/// 原子状态已经由写守卫更新；这里只把同一份事实送给桥接视图，不再写 atom。
fn notify_current_state() {
    let state = atoms::HISTORY_V2_STATE.state().read().clone();
    notify_view(&state);
}

#[cfg(test)]
#[path = "consumer_test.rs"]
mod consumer_test;
