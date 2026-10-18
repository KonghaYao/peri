//! S5.4 窗口请求/结算状态机：open/page/seek、单在途 + 单待合并、迟到页丢弃。
//!
//! 只有 `HISTORY_WINDOW_V1` 开关与连接能力同时为真时启用；否则旧 `HISTORY_PAGING`
//! 路径保持不变。桥接任务拥有 `HistoryWindow`，本模块只维护客户端侧请求身份、预留账目
//! 与在途/待合并状态。

//! 触发侧（滚动/gap 恢复）在 S5.4/S5.6 接线前由单元测试覆盖；保留未使用告警豁免。
#![allow(dead_code)]

use std::time::Instant;

use serde_json::Value;
use tracing::{debug, warn};

use crate::acp_client::AcpTuiClient;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms;
use crate::kit::history_paging::WindowGeneration;
use crate::kit::history_paging::{
    self, WINDOW_DEFAULT_BYTE_BUDGET, WINDOW_PAGE_LIMIT, WindowDirection, WindowOpenRequest,
    WindowPageCursor, WindowPageRequest, WindowPageResponse, WindowRequest, WindowRequestAnchor,
    WindowSeekRequest,
};
use crate::kit::history_window::{
    PageReservation, PersistentEntryKey, WindowIdentity, WindowLimits,
};

#[path = "window_triggers.rs"]
mod triggers;
pub use triggers::*;

const WINDOW_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const RETRY_BASE_MS: u64 = 1_000;
const RETRY_MAX_MS: u64 = 30_000;

/// 页在桥接布局里的插入方向。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowPagePlacement {
    #[default]
    Tail,
    BeforeFirst,
}

/// 一次窗口请求的本地身份。`requestId` 不进入响应，由 consumer 自己配对。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRequestContext {
    pub request_id: String,
    pub session_id: String,
    pub identity: WindowIdentity,
    pub window_generation: u64,
    pub reserved_bytes: usize,
    pub placement: WindowPagePlacement,
    /// Open 响应没有先验 snapshot，跳过 wire 身份比对，只校验 session。
    pub is_open: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowConsumerRequest {
    Open {
        request_id: String,
        session_id: String,
        direction: WindowDirection,
        limit: u16,
        byte_budget: u32,
    },
    Page {
        context: WindowRequestContext,
        cursor: WindowPageCursor,
        limit: u16,
    },
    Seek {
        context: WindowRequestContext,
        anchor: PersistentEntryKey,
        before_limit: u16,
        after_limit: u16,
    },
}

impl WindowConsumerRequest {
    fn request_id(&self) -> &str {
        match self {
            Self::Open { request_id, .. } => request_id,
            Self::Page { context, .. } | Self::Seek { context, .. } => &context.request_id,
        }
    }

    pub(crate) fn session_id(&self) -> &str {
        match self {
            Self::Open { session_id, .. } => session_id,
            Self::Page { context, .. } | Self::Seek { context, .. } => &context.session_id,
        }
    }

    fn reserved_bytes(&self) -> usize {
        match self {
            Self::Open { .. } => WindowLimits::default().page_target_bytes,
            Self::Page { context, .. } | Self::Seek { context, .. } => context.reserved_bytes,
        }
    }

    fn kind(&self) -> WindowInFlightKind {
        match self {
            Self::Open {
                direction, limit, ..
            } => WindowInFlightKind::Open {
                direction: *direction,
                limit: *limit,
            },
            Self::Page { cursor, limit, .. } => WindowInFlightKind::Page {
                cursor: cursor.clone(),
                limit: *limit,
            },
            Self::Seek {
                anchor,
                before_limit,
                after_limit,
                ..
            } => WindowInFlightKind::Seek {
                anchor: anchor.clone(),
                before_limit: *before_limit,
                after_limit: *after_limit,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowInFlightKind {
    Open {
        direction: WindowDirection,
        limit: u16,
    },
    Page {
        cursor: WindowPageCursor,
        limit: u16,
    },
    Seek {
        anchor: PersistentEntryKey,
        before_limit: u16,
        after_limit: u16,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInFlight {
    pub context: WindowRequestContext,
    pub kind: WindowInFlightKind,
    pub reservation: PageReservation,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WindowPagingStatus {
    #[default]
    Ready,
    Degraded {
        reason: String,
        retry_at: Option<Instant>,
        attempts: u32,
    },
}

impl WindowPagingStatus {
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowPagingState {
    pub session_id: String,
    pub identity: Option<WindowIdentity>,
    pub window_generation: u64,
    pub read_ticket: Option<String>,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    pub in_flight: Option<WindowInFlight>,
    pub pending_merge: Option<WindowInFlight>,
    pub reserved_bytes: usize,
    pub status: WindowPagingStatus,
    pub last_error: Option<String>,
    pub last_request_at: Option<Instant>,
    pub next_request_seq: u64,
    /// after 页已到 durable 末尾；open/reset 清除。
    pub newer_exhausted: bool,
}

impl WindowPagingState {
    pub fn accepts_identity(&self, identity: &WindowIdentity) -> bool {
        self.identity.as_ref() == Some(identity)
    }

    pub fn is_degraded(&self) -> bool {
        self.status.is_degraded()
    }

    fn reserve_limit(&self) -> usize {
        WindowLimits::default().reserved_bytes
    }
}

// `Granted` 携带完整 in-flight 上下文；其余变体只是状态标签。boxing 会改动所有匹配点，
// 收益不抵改动面。
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowClaim {
    Granted(WindowInFlight),
    Merged,
    Disabled,
    Rejected(WindowClaimRejection),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowClaimRejection {
    Degraded {
        reason: String,
    },
    InFlight,
    PendingMerge,
    ReservationBudget {
        used: usize,
        limit: usize,
        requested: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowSettlement {
    Accepted(Box<WindowInFlight>),
    AcceptedEmpty,
    Stale,
    Unmatched,
    ReopenRequired,
}

pub fn window_path_enabled() -> bool {
    atoms::HISTORY_WINDOW_V1.get() && atoms::HISTORY_WINDOW_CAPABLE.get()
}

pub fn reset_window_paging_state() {
    *atoms::HISTORY_WINDOW_PAGING.state().write() = WindowPagingState::default();
    atoms::HISTORY_WINDOW_GENERATION.set(0);
}

/// 桥接窗口 `begin_window` 后同步给 consumer；后续页请求使用这个代次。
pub fn note_window_generation(generation: u64) {
    atoms::HISTORY_WINDOW_GENERATION.set(generation);
    atoms::HISTORY_WINDOW_PAGING
        .state()
        .write()
        .window_generation = generation;
}

pub fn build_window_open_request(direction: WindowDirection) -> WindowConsumerRequest {
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    let request_id = next_request_id(&mut state);
    WindowConsumerRequest::Open {
        request_id,
        session_id: state.session_id.clone(),
        direction,
        limit: WINDOW_PAGE_LIMIT,
        byte_budget: WINDOW_DEFAULT_BYTE_BUDGET,
    }
}

/// 会话 load 成功后启动窗口浏览。返回是否成功入队。
pub(crate) fn enqueue_window_open(session_id: &str, direction: WindowDirection) -> bool {
    if !window_path_enabled() {
        return false;
    }
    reset_window_paging_state();
    let request = {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let mut state = handle.write();
        state.session_id = session_id.to_string();
        drop(state);
        build_window_open_request(direction)
    };
    atoms::HISTORY_WINDOW_TX
        .get()
        .is_some_and(|tx| tx.send(request).is_ok())
}

pub fn build_window_page_request(
    cursor: WindowPageCursor,
    limit: u16,
) -> Option<WindowConsumerRequest> {
    if !window_path_enabled() {
        return None;
    }
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    if state.is_degraded() {
        return None;
    }
    let identity = state.identity.clone()?;
    let request_id = next_request_id(&mut state);
    let placement = match cursor {
        WindowPageCursor::Before(_) => WindowPagePlacement::BeforeFirst,
        WindowPageCursor::After(_) => WindowPagePlacement::Tail,
    };
    let generation = current_window_generation(&state);
    Some(WindowConsumerRequest::Page {
        context: WindowRequestContext {
            request_id,
            session_id: identity.session_id.clone(),
            identity,
            window_generation: generation,
            reserved_bytes: WindowLimits::default().page_target_bytes,
            placement,
            is_open: false,
        },
        cursor,
        limit,
    })
}

pub fn build_window_seek_request(
    anchor: PersistentEntryKey,
    before_limit: u16,
    after_limit: u16,
) -> Option<WindowConsumerRequest> {
    if !window_path_enabled() {
        return None;
    }
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    if state.is_degraded() {
        return None;
    }
    let identity = state.identity.clone()?;
    let request_id = next_request_id(&mut state);
    let generation = current_window_generation(&state);
    Some(WindowConsumerRequest::Seek {
        context: WindowRequestContext {
            request_id,
            session_id: identity.session_id.clone(),
            identity,
            window_generation: generation,
            reserved_bytes: WindowLimits::default().page_target_bytes,
            placement: WindowPagePlacement::Tail,
            is_open: false,
        },
        anchor,
        before_limit,
        after_limit,
    })
}

fn next_request_id(state: &mut WindowPagingState) -> String {
    state.next_request_seq = state.next_request_seq.saturating_add(1);
    format!("window-{}", state.next_request_seq)
}

/// 桥接代次是权威；consumer 缓存可能滞后（open 响应 settlement 早于 bridge 处理）。
fn current_window_generation(state: &WindowPagingState) -> u64 {
    let atom = atoms::HISTORY_WINDOW_GENERATION.get();
    if atom > 0 {
        atom
    } else {
        state.window_generation
    }
}

/// S5.6：锚点落在 gap 时先占位，再按 gap 游标/稳定身份生成重读请求。
pub fn claim_window_request(request: &WindowConsumerRequest) -> WindowClaim {
    if !window_path_enabled() {
        return WindowClaim::Disabled;
    }
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    let is_open = matches!(request, WindowConsumerRequest::Open { .. });
    if let WindowPagingStatus::Degraded { reason, .. } = &state.status
        && !is_open
    {
        return WindowClaim::Rejected(WindowClaimRejection::Degraded {
            reason: reason.clone(),
        });
    }
    if let Some(in_flight) = &state.in_flight {
        if in_flight.context.request_id == request.request_id() || in_flight.kind == request.kind()
        {
            return WindowClaim::Merged;
        }
        return WindowClaim::Rejected(WindowClaimRejection::InFlight);
    }
    if state.pending_merge.is_some() {
        return WindowClaim::Rejected(WindowClaimRejection::PendingMerge);
    }
    let reserved = request.reserved_bytes();
    if state.reserved_bytes.saturating_add(reserved) > state.reserve_limit() {
        return WindowClaim::Rejected(WindowClaimRejection::ReservationBudget {
            used: state.reserved_bytes,
            limit: state.reserve_limit(),
            requested: reserved,
        });
    }
    let (context, kind) = match request {
        WindowConsumerRequest::Open {
            request_id,
            session_id,
            direction,
            limit,
            ..
        } => (
            WindowRequestContext {
                request_id: request_id.clone(),
                session_id: session_id.clone(),
                identity: WindowIdentity::new(session_id.clone(), "", 0, ""),
                window_generation: state.window_generation,
                reserved_bytes: reserved,
                placement: WindowPagePlacement::Tail,
                is_open: true,
            },
            WindowInFlightKind::Open {
                direction: *direction,
                limit: *limit,
            },
        ),
        WindowConsumerRequest::Page {
            context,
            cursor,
            limit,
        } => (
            context.clone(),
            WindowInFlightKind::Page {
                cursor: cursor.clone(),
                limit: *limit,
            },
        ),
        WindowConsumerRequest::Seek {
            context,
            anchor,
            before_limit,
            after_limit,
        } => (
            context.clone(),
            WindowInFlightKind::Seek {
                anchor: anchor.clone(),
                before_limit: *before_limit,
                after_limit: *after_limit,
            },
        ),
    };
    let reservation = PageReservation {
        request_id: context.request_id.clone(),
        window_generation: context.window_generation,
        bytes: reserved,
    };
    let in_flight = WindowInFlight {
        context,
        kind,
        reservation,
    };
    state.in_flight = Some(in_flight.clone());
    state.reserved_bytes = state.reserved_bytes.saturating_add(reserved);
    state.last_request_at = Some(Instant::now());
    WindowClaim::Granted(in_flight)
}

pub fn settle_window_response(
    in_flight: &WindowInFlight,
    response: &WindowPageResponse,
) -> WindowSettlement {
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    let Some(current) = state.in_flight.clone() else {
        return WindowSettlement::Unmatched;
    };
    if current.context.request_id != in_flight.context.request_id {
        return WindowSettlement::Unmatched;
    }
    let generation = WindowGeneration {
        session_id: current.context.identity.session_id.clone(),
        session_generation: current.context.identity.session_generation.clone(),
        structure_version: current.context.identity.structure_version,
        snapshot_token: current.context.identity.snapshot_token.clone(),
        window_generation: current.context.window_generation,
        request_id: current.context.request_id.clone(),
    };
    let is_open = matches!(current.kind, WindowInFlightKind::Open { .. });
    let identity_ok = if is_open {
        response.session_id == current.context.session_id
    } else {
        generation.accepts(response, state.window_generation)
    };
    if !identity_ok {
        state.in_flight = None;
        state.reserved_bytes = state
            .reserved_bytes
            .saturating_sub(current.reservation.bytes);
        return WindowSettlement::Stale;
    }
    if response.reopen_required {
        state.in_flight = None;
        state.reserved_bytes = state
            .reserved_bytes
            .saturating_sub(current.reservation.bytes);
        state.status = WindowPagingStatus::Degraded {
            reason: "history window requires reopen".into(),
            retry_at: Some(Instant::now() + retry_delay(1)),
            attempts: 1,
        };
        let session_id = state.session_id.clone();
        let reason = state
            .last_error
            .clone()
            .unwrap_or_else(|| "history window requires reopen".into());
        drop(state);
        emit_window_degraded(&session_id, &reason);
        return WindowSettlement::ReopenRequired;
    }
    state.in_flight = None;
    state.identity = Some(response.identity.clone());
    state.read_ticket = Some(response.read_ticket.clone());
    state.before_cursor = response.before_cursor.clone();
    state.after_cursor = response.after_cursor.clone();
    if is_open {
        // 成功的 open 是唯一能把 consumer 从 Degraded 拉回 Ready 的路径。
        state.status = WindowPagingStatus::Ready;
        state.last_error = None;
        state.newer_exhausted = false;
    } else if response.items.is_empty()
        && let WindowInFlightKind::Page {
            cursor: WindowPageCursor::After(request_cursor),
            ..
        } = &current.kind
        && response
            .after_cursor
            .as_deref()
            .is_none_or(|next| next == request_cursor)
    {
        state.newer_exhausted = true;
    }
    let generation = atoms::HISTORY_WINDOW_GENERATION.get();
    if generation > 0 {
        state.window_generation = generation;
    } else {
        state.window_generation = state.window_generation.saturating_add(1);
    }
    if response.items.is_empty() {
        state.reserved_bytes = state
            .reserved_bytes
            .saturating_sub(current.reservation.bytes);
        return WindowSettlement::AcceptedEmpty;
    }
    state.pending_merge = Some(current.clone());
    WindowSettlement::Accepted(Box::new(current))
}

/// bridge 发布后结算待合并页：归还 consumer 侧预留。
pub fn finish_window_merge(request_id: &str) -> usize {
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    let Some(pending) = state.pending_merge.clone() else {
        return 0;
    };
    if pending.context.request_id != request_id {
        return 0;
    }
    state.pending_merge = None;
    state.reserved_bytes = state
        .reserved_bytes
        .saturating_sub(pending.reservation.bytes);
    pending.reservation.bytes
}

/// 请求失败/桥接拒绝：归还预留；失效错误进入 Degraded 并安排 open 重试。
pub fn fail_window_request(request_id: &str, message: impl Into<String>, invalidates: bool) {
    let message = message.into();
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    let session_id = state.session_id.clone();
    let mut released = 0usize;
    if let Some(in_flight) = state.in_flight.clone()
        && in_flight.context.request_id == request_id
    {
        released = released.saturating_add(in_flight.reservation.bytes);
        state.in_flight = None;
    }
    if let Some(pending) = state.pending_merge.clone()
        && pending.context.request_id == request_id
    {
        released = released.saturating_add(pending.reservation.bytes);
        state.pending_merge = None;
    }
    state.reserved_bytes = state.reserved_bytes.saturating_sub(released);
    state.last_error = Some(message.clone());
    if invalidates {
        let attempts = match &state.status {
            WindowPagingStatus::Degraded { attempts, .. } => attempts.saturating_add(1),
            WindowPagingStatus::Ready => 1,
        };
        state.status = WindowPagingStatus::Degraded {
            reason: message,
            retry_at: Some(Instant::now() + retry_delay(attempts)),
            attempts,
        };
    }
    let reason = state.last_error.clone().unwrap_or_default();
    drop(state);
    if invalidates {
        emit_window_degraded(&session_id, &reason);
    }
}

fn emit_window_degraded(session_id: &str, reason: &str) {
    if session_id.is_empty() {
        return;
    }
    let Some(tx) = atoms::LOCAL_EVENT_TX.get() else {
        return;
    };
    let _ = tx.send(AcpEventWithEpoch {
        event: AcpEventData::HistoryWindowDegraded {
            session_id: session_id.to_string(),
            reason: reason.to_string(),
        },
        active_session_id: session_id.to_string(),
    });
}

fn retry_delay(attempts: u32) -> std::time::Duration {
    let shift = attempts.saturating_sub(1).min(5);
    let millis = RETRY_BASE_MS
        .saturating_mul(1u64 << shift)
        .min(RETRY_MAX_MS);
    std::time::Duration::from_millis(millis)
}

/// Degraded 到点后生成一次 `open(latest)` 重试；不自动退化为完整 `session/load`。
pub(crate) fn window_request_wire(
    request: &WindowConsumerRequest,
    in_flight: &WindowInFlight,
) -> Option<WindowRequest> {
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let state = handle.read();
    match request {
        WindowConsumerRequest::Open {
            direction,
            limit,
            byte_budget,
            ..
        } => Some(WindowRequest::Open(WindowOpenRequest {
            anchor: WindowRequestAnchor::Latest,
            direction: *direction,
            limit: *limit,
            before_limit: None,
            after_limit: None,
            byte_budget: *byte_budget,
        })),
        WindowConsumerRequest::Page { cursor, limit, .. } => {
            let read_ticket = state.read_ticket.clone()?;
            Some(WindowRequest::Page(WindowPageRequest {
                snapshot_token: in_flight.context.identity.snapshot_token.clone(),
                read_ticket,
                cursor: cursor.clone(),
                limit: *limit,
            }))
        }
        WindowConsumerRequest::Seek {
            anchor,
            before_limit,
            after_limit,
            ..
        } => {
            let read_ticket = state.read_ticket.clone()?;
            Some(WindowRequest::Seek(WindowSeekRequest {
                snapshot_token: in_flight.context.identity.snapshot_token.clone(),
                read_ticket,
                anchor: anchor.clone(),
                before_limit: *before_limit,
                after_limit: *after_limit,
            }))
        }
    }
}

pub fn spawn_history_window_consumer(
    client: AcpTuiClient,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<WindowConsumerRequest>,
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
            let claim = claim_window_request(&request);
            let in_flight = match claim {
                WindowClaim::Granted(in_flight) => in_flight,
                WindowClaim::Merged => {
                    let handle = atoms::HISTORY_WINDOW_PAGING.state();
                    let state = handle.read();
                    match state.in_flight.clone() {
                        Some(in_flight) if in_flight.context.request_id == request.request_id() => {
                            in_flight
                        }
                        _ => continue,
                    }
                }
                other => {
                    debug!(
                        request_id = %request.request_id(),
                        "history_window_consumer: request not granted ({other:?})"
                    );
                    continue;
                }
            };
            let Some(wire) = window_request_wire(&request, &in_flight) else {
                fail_window_request(
                    &in_flight.context.request_id,
                    "window read ticket or snapshot is missing",
                    false,
                );
                continue;
            };
            let session_id = request.session_id().to_string();
            let response = peri_time::timeout(
                WINDOW_FETCH_TIMEOUT,
                client.load_history_window(&session_id, &wire),
            )
            .await;
            match response {
                Ok(Ok(raw)) => {
                    let Some(decoded) = history_paging::decode_window_page_response(&raw) else {
                        fail_window_request(
                            &in_flight.context.request_id,
                            "history window response has an unexpected shape",
                            false,
                        );
                        continue;
                    };
                    match settle_window_response(&in_flight, &decoded) {
                        WindowSettlement::Accepted(_) | WindowSettlement::AcceptedEmpty => {
                            if let Some(tx) = atoms::LOCAL_EVENT_TX.get() {
                                let _ = tx.send(AcpEventWithEpoch {
                                    event: AcpEventData::HistoryWindowPageLoaded {
                                        context: Box::new(in_flight.context.clone()),
                                        response: Box::new(decoded),
                                    },
                                    active_session_id: session_id,
                                });
                            } else {
                                fail_window_request(
                                    &in_flight.context.request_id,
                                    "local bridge channel is unavailable",
                                    false,
                                );
                            }
                        }
                        WindowSettlement::Stale | WindowSettlement::Unmatched => {
                            debug!(
                                request_id = %in_flight.context.request_id,
                                "history_window_consumer: stale response dropped"
                            );
                        }
                        WindowSettlement::ReopenRequired => {
                            warn!(
                                request_id = %in_flight.context.request_id,
                                "history_window_consumer: server requires reopen"
                            );
                        }
                    }
                }
                Ok(Err(error)) => {
                    if error.code == -32601 {
                        atoms::HISTORY_WINDOW_CAPABLE.set(false);
                    }
                    let invalidates =
                        matches!(error.code, -32011 | -32012 | -32014 | -32015 | -32601);
                    fail_window_request(
                        &in_flight.context.request_id,
                        error.to_string(),
                        invalidates,
                    );
                }
                Err(_) => {
                    fail_window_request(
                        &in_flight.context.request_id,
                        format!("history window RPC timed out after {WINDOW_FETCH_TIMEOUT:?}"),
                        false,
                    );
                }
            }
        }
    })
}

/// 失败原因里是否包含语义错误码（供测试/诊断）。
pub(crate) fn is_invalidating_code(code: i32) -> bool {
    matches!(code, -32011 | -32012 | -32014 | -32015 | -32601)
}

/// Degraded 退避到点时通过滚动触发一次 open 重试（不自动完整 load）。
pub(crate) fn window_error_data(value: &Value) -> Option<String> {
    value
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
}
