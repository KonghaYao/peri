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
use std::sync::atomic::Ordering;
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
}

static OPEN_IN_FLIGHT: Mutex<Option<OpenClaim>> = Mutex::new(None);

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
    atoms::HISTORY_V2.get() && atoms::HISTORY_V2_CAPABLE.get()
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

/// H5c：详情面板的正文请求。详情是窗口内容的派生阅读器：按 §11.4 使用独立的
/// 详情预算和随机 offset 语义，不写窗口页/正文账目，也不参与窗口代次结算；
/// 响应由 [`HistoryV2BodyBinding`] 的 window/snapshot/bodyRef 校验，导航代次变化
/// 时投影层会直接收起旧详情。`requestKey` 仍与页/seek 共用连接级单调序列。
pub(crate) fn body_request(
    binding: &crate::kit::history_window::HistoryV2BodyBinding,
    offset: u64,
    max_bytes: u32,
) -> super::request::HistoryV2BodyRequest {
    super::request::HistoryV2BodyRequest {
        context: super::request::HistoryV2RequestContext {
            request_key: next_request_key(),
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
    }
}

/// 会话边界复位；在途 open 随连接代次作废。
pub(crate) fn reset_consumer_state() {
    *open_claim_slot() = None;
    atoms::HISTORY_V2_PREFIX_LEN.set(0);
    atoms::HISTORY_V2_VIEW_EPOCH.set(0);
}

fn open_claim_slot() -> std::sync::MutexGuard<'static, Option<OpenClaim>> {
    OPEN_IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn try_claim_open(session_id: &str) -> Option<OpenClaim> {
    let mut slot = open_claim_slot();
    if slot.is_some() {
        return None;
    }
    let claim = OpenClaim {
        session_id: session_id.to_owned(),
        request_key: next_request_key(),
    };
    *slot = Some(claim.clone());
    Some(claim)
}

fn release_open_claim(claim: &OpenClaim) {
    let mut slot = open_claim_slot();
    if slot.as_ref() == Some(claim) {
        *slot = None;
    }
}

fn release_open_for(session_id: &str, request_key: DecimalU64) {
    release_open_claim(&OpenClaim {
        session_id: session_id.to_owned(),
        request_key,
    });
}

#[cfg(test)]
fn open_claim_is_some() -> bool {
    open_claim_slot().is_some()
}

pub(crate) fn notify_view(state: &HistoryV2State) {
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
    let Some(request) = begin_page(direction, &limits) else {
        return false;
    };
    send(HistoryV2ConsumerRequest::Page(request))
}

/// 在 reducer 写锁内认领一页；Busy/Exhausted/ReopenRequired 都只返回 `None`。
fn begin_page(direction: HistoryDirection, limits: &HistoryLimits) -> Option<HistoryV2PageRequest> {
    let handle = atoms::HISTORY_V2_STATE.state();
    let mut state = handle.write();
    if !state.is_ready() {
        return None;
    }
    let window = state.window.as_mut()?;
    let request_key = next_request_key();
    match window.request_page(request_key, direction, limits.default_items, limits) {
        Ok(request) => Some(request),
        Err(rejection) => {
            debug!(?direction, ?rejection, "history_v2: page trigger rejected");
            None
        }
    }
}

/// 定位锚点后发出 seek；锚点身份来自当前保留窗口的稳定条目。
pub(crate) fn request_seek_if_due(anchor: HistoryEntryAnchor) -> bool {
    if !v2_path_active() {
        return false;
    }
    let limits = current_limits();
    let Some(request) = begin_seek(&anchor, &limits) else {
        return false;
    };
    send(HistoryV2ConsumerRequest::Seek(request))
}

fn begin_seek(anchor: &HistoryEntryAnchor, limits: &HistoryLimits) -> Option<HistoryV2SeekRequest> {
    let handle = atoms::HISTORY_V2_STATE.state();
    let mut state = handle.write();
    if !state.is_ready() {
        return None;
    }
    let window = state.window.as_mut()?;
    let before = limits.default_items / 2;
    let after = limits.default_items.saturating_sub(before);
    let request_key = next_request_key();
    match window.request_seek(request_key, anchor, before, after, limits) {
        Ok(request) => Some(request),
        Err(rejection) => {
            debug!(?rejection, "history_v2: seek trigger rejected");
            None
        }
    }
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
    let Some(claim) = try_claim_open(&session_id) else {
        return false;
    };
    // F2b：重开是替换窗口。先 best-effort 关闭旧窗口，再入队 Open；关闭失败不阻塞
    // 新窗口建立（旧窗口最终由服务端 TTL/连接关闭回收）。
    let _ = enqueue_close(None);
    let request = HistoryV2ConsumerRequest::Open {
        session_id,
        request_key: claim.request_key,
        anchor,
    };
    send_with_claim(request, claim)
}

fn send(request: HistoryV2ConsumerRequest) -> bool {
    atoms::HISTORY_V2_TX
        .get()
        .is_some_and(|tx| tx.send(request).is_ok())
}

/// 新会话首个 V2 窗口：等价于失败恢复用的 open，但允许在空状态上建立窗口。
///
/// 不伪造 `session/load` initial；服务端 `open` 返回的页面就是首窗。
pub(crate) fn request_open_latest(session_id: &str) -> bool {
    if !v2_path_active() || session_id.is_empty() {
        return false;
    }
    let Some(claim) = try_claim_open(session_id) else {
        return false;
    };
    // F2b：已有窗口时先入队 Close（旧 window/snapshot/ticket），再入队 Open；
    // 避免同会话反复 open 累积窗口。close 失败只是 best-effort 未发送。
    let _ = enqueue_close(None);
    let request = HistoryV2ConsumerRequest::Open {
        session_id: session_id.to_owned(),
        request_key: claim.request_key,
        anchor: None,
    };
    send_with_claim(request, claim)
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

fn enqueue_close(expected_session: Option<&str>) -> bool {
    let request = {
        let handle = atoms::HISTORY_V2_STATE.state();
        let state = handle.read();
        if !state.is_ready() {
            return false;
        }
        let Some(session_id) = state.session_id.clone() else {
            return false;
        };
        if expected_session.is_some_and(|expected| expected != session_id) {
            return false;
        }
        let Some(window) = state.window.as_ref() else {
            return false;
        };
        HistoryV2ConsumerRequest::Close {
            session_id,
            request_key: next_request_key(),
            window_id: window.window_id.clone(),
            snapshot_token: window.snapshot_token.clone(),
            read_ticket: window.read_ticket.clone(),
        }
    };
    let sent = send(request);
    if !sent {
        debug!("history_v2: close request could not be enqueued");
    }
    sent
}

fn send_with_claim(request: HistoryV2ConsumerRequest, claim: OpenClaim) -> bool {
    let sent = send(request);
    if !sent {
        release_open_claim(&claim);
    }
    sent
}

/// 结算一页；只有 reducer 明确接纳才发布视图。
pub(crate) fn settle_page(
    state: &mut HistoryV2State,
    request: &HistoryV2PageRequest,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> bool {
    let accepted = state.window.as_mut().is_some_and(|window| {
        matches!(
            window.settle_page(request, page, limits),
            super::paging::HistoryV2PageSettlement::Accepted { .. }
                | super::paging::HistoryV2PageSettlement::Empty { .. }
        )
    });
    if accepted {
        publish_state(state.clone());
    }
    accepted
}

pub(crate) fn settle_seek(
    state: &mut HistoryV2State,
    request: &HistoryV2SeekRequest,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> bool {
    let accepted = state.window.as_mut().is_some_and(|window| {
        matches!(
            window.settle_seek(request, page, limits),
            HistoryV2SeekSettlement::Accepted { .. }
        )
    });
    if accepted {
        publish_state(state.clone());
    }
    accepted
}

/// 失败结算。返回是否允许后续用新 requestKey 重试。
///
/// 迟到失败只允许作用于发出它的 session/window/requestKey；任一不匹配都整体忽略，
/// 不释放在途标记、不改窗口状态。
pub(crate) fn apply_failure(
    state: &mut HistoryV2State,
    identity: HistoryV2RequestIdentity<'_>,
    failure: &HistoryV2WireFailure,
) -> bool {
    if !identity.matches(state) {
        debug!(
            session_id = identity.session_id,
            "history_v2: ignoring a failure from another session/window"
        );
        return false;
    }
    let Some(window) = state.window.as_mut() else {
        return false;
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
        return false;
    }
    if failure.operation_in_progress() {
        // 原操作仍在服务端执行：保留在途标记，触发侧不会重复分配。
        window.paging.last_error = Some(failure.label().to_owned());
        publish_state(state.clone());
        return false;
    }
    let reopen = failure.requires_reopen();
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
        window.paging.status = if reopen {
            HistoryV2PagingStatus::ReopenRequired {
                reason: failure.label().to_owned(),
            }
        } else {
            HistoryV2PagingStatus::Retryable {
                reason: failure.label().to_owned(),
            }
        };
    }
    publish_state(state.clone());
    !reopen
}

/// 传输失败/超时：释放当前在途并允许重试；跨会话/窗口的迟到失败直接忽略。
pub(crate) fn apply_transport_failure(
    state: &mut HistoryV2State,
    identity: HistoryV2RequestIdentity<'_>,
    label: &str,
) {
    if !identity.matches(state) {
        return;
    }
    if let Some(window) = state.window.as_mut() {
        window.fail_request(identity.request_key, label, false);
    }
    publish_state(state.clone());
}

/// open 失败（域错误/传输/解析）：先归还本次 claim，再只结算同会话的当前窗口。
fn settle_open_failure(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    label: &str,
) {
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
        return;
    }
    if let Some(window) = state.window.as_mut() {
        window.paging.status = HistoryV2PagingStatus::Retryable {
            reason: label.to_owned(),
        };
        window.paging.last_error = Some(label.to_owned());
    }
    publish_state(state.clone());
}

fn settle_open(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) {
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
        return;
    }
    let previous = state.clone();
    // 空状态 = `session/new` 的首窗；已有状态 = 失效后的重建。
    let admitted = if previous.session_id.is_none() && previous.window.is_none() {
        initial::admit_new_session_open(session_id, page, limits, request_key)
    } else {
        initial::admit_open_response(&previous, session_id, page, limits, request_key)
    };
    match admitted {
        Ok(next) => publish_state(next),
        Err(error) => {
            if let Some(window) = state.window.as_mut() {
                window.paging.status = HistoryV2PagingStatus::Retryable {
                    reason: error.to_string(),
                };
                window.paging.last_error = Some(error.to_string());
            }
            warn!(reason = %error, "history_v2: reopen response rejected");
            publish_state(state.clone());
        }
    }
}

/// 启动 V2 消费任务。只有与 V1 队列互斥的触发侧会向这里投递。
pub fn spawn_history_v2_consumer(
    client: AcpTuiClient,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<HistoryV2ConsumerRequest>,
    shutdown: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let request = tokio::select! {
                _ = shutdown.cancelled() => break,
                request = rx.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
            };
            let limits = current_limits();
            let identity = request.identity();
            let params = request.params(&limits);
            let response =
                peri_time::timeout(FETCH_TIMEOUT, client.history_v2_request(params)).await;
            if matches!(&request, HistoryV2ConsumerRequest::Close { .. }) {
                // best-effort：关闭失败也不回滚/伪造本地状态；服务端会话拆除仍会清理。
                match &response {
                    Ok(Ok(_)) => debug!("history_v2: window close acknowledged"),
                    Ok(Err(error)) => {
                        debug!(%error, "history_v2: window close rejected")
                    }
                    Err(_) => debug!("history_v2: window close timed out"),
                }
                continue;
            }
            match response {
                Ok(Ok(raw)) => {
                    let page = match parse_page_result(&raw) {
                        Ok(page) => page,
                        Err(_) => {
                            let handle = atoms::HISTORY_V2_STATE.state();
                            let mut state = handle.write();
                            if matches!(&request, HistoryV2ConsumerRequest::Open { .. }) {
                                settle_open_failure(
                                    &mut state,
                                    identity.session_id,
                                    identity.request_key,
                                    "invalid_page_shape",
                                );
                            } else {
                                apply_transport_failure(&mut state, identity, "invalid_page_shape");
                            }
                            continue;
                        }
                    };
                    let handle = atoms::HISTORY_V2_STATE.state();
                    let mut state = handle.write();
                    match &request {
                        HistoryV2ConsumerRequest::Open {
                            session_id,
                            request_key,
                            ..
                        } => {
                            release_open_for(session_id, *request_key);
                            settle_open(&mut state, session_id, *request_key, &page, &limits);
                        }
                        HistoryV2ConsumerRequest::Page(page_request) => {
                            settle_page(&mut state, page_request, &page, &limits);
                        }
                        HistoryV2ConsumerRequest::Seek(seek_request) => {
                            settle_seek(&mut state, seek_request, &page, &limits);
                        }
                        // Close 已在解析页之前处理并 continue。
                        HistoryV2ConsumerRequest::Close { .. } => {}
                    }
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
                    let handle = atoms::HISTORY_V2_STATE.state();
                    let mut state = handle.write();
                    apply_failure(&mut state, identity, &failure);
                }
                Err(_) => {
                    let handle = atoms::HISTORY_V2_STATE.state();
                    let mut state = handle.write();
                    if matches!(&request, HistoryV2ConsumerRequest::Open { .. }) {
                        settle_open_failure(
                            &mut state,
                            identity.session_id,
                            identity.request_key,
                            "timeout",
                        );
                    } else {
                        apply_transport_failure(&mut state, identity, "timeout");
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[path = "consumer_test.rs"]
mod consumer_test;
