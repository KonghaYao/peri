//! V2 窗口结算：page/seek/failure/open 对状态的修改，不做发布。
//!
//! 发布（`publish_state`/`notify_view`）必须在写锁之外进行；本模块的函数只接受调用方
//! 已持有的 `&mut HistoryV2State`，由 `consumer.rs` 或测试入口在锁释放后通知视图。

use peri_acp_types::history_v2::{DecimalU64, HistoryLimits, HistoryWindowPage};
use tracing::{debug, warn};

use super::super::HistoryV2State;
use super::super::error::HistoryV2WireFailure;
use super::super::paging::{
    HistoryV2PageSettlement, HistoryV2PagingStatus, HistoryV2SeekSettlement,
};
use super::super::request::{HistoryV2PageRequest, HistoryV2RequestIdentity, HistoryV2SeekRequest};
#[cfg(test)]
use super::publish_state;
use super::support::HistoryV2WindowIdentity;
use super::{consumer_epoch, release_open_for};

/// 结算一页的核心：只修改调用方持有的状态，不发布（发布需要原子写锁之外的
/// 上下文；在写锁内调用 `Atom::set` 会自我阻塞）。
pub(super) fn apply_page(
    state: &mut HistoryV2State,
    request: &HistoryV2PageRequest,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> bool {
    state.window.as_mut().is_some_and(|window| {
        matches!(
            window.settle_page(request, page, limits),
            HistoryV2PageSettlement::Accepted { .. } | HistoryV2PageSettlement::Empty { .. }
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

pub(super) fn apply_seek(
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
pub(super) fn apply_failure_core(
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
pub(super) fn apply_transport_failure_core(
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
pub(super) fn apply_open_failure(
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
pub(crate) fn settle_open_failure(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    label: &str,
) {
    if apply_open_failure(state, session_id, request_key, label) {
        publish_state(state.clone());
    }
}

pub(super) enum OpenSettlement {
    /// 迟到/不属于当前会话：不改状态。
    Unchanged,
    /// 已修改状态；成功接纳时携带待关闭的旧窗口身份。
    Changed {
        close: Option<HistoryV2WindowIdentity>,
        /// 新窗口已接纳（而不是被拒绝后标为需重开）。
        accepted: bool,
    },
}

pub(super) fn apply_open(
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
        super::super::initial::admit_new_session_open(session_id, page, limits, request_key)
    } else {
        super::super::initial::admit_open_response(&previous, session_id, page, limits, request_key)
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
                accepted: true,
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
            OpenSettlement::Changed {
                close: None,
                accepted: false,
            }
        }
    }
}

/// 测试/同步调用入口：修改后发布，返回接纳成功后的旧窗口身份。
#[cfg(test)]
pub(crate) fn settle_open(
    state: &mut HistoryV2State,
    session_id: &str,
    request_key: DecimalU64,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
) -> Option<HistoryV2WindowIdentity> {
    match apply_open(state, session_id, request_key, None, page, limits) {
        OpenSettlement::Unchanged => None,
        OpenSettlement::Changed { close, .. } => {
            publish_state(state.clone());
            close
        }
    }
}
