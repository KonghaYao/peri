//! S5.4/S5.6 窗口请求触发：before/after/gap 与 Degraded open 重试。
//!
//! 与请求状态机共用同一单在途/单待合并账本；触发只负责判定与入队。

use std::time::Instant;

use tracing::debug;

use super::*;
use crate::kit::history_paging::WindowGapRequest;
use crate::kit::history_window::GapSeekDirection;

pub fn request_window_older_page_if_due() -> bool {
    if !window_path_enabled() {
        return false;
    }
    let cursor = {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let state = handle.read();
        if state.is_degraded() {
            drop(state);
            return request_window_retry_if_due();
        }
        if state.in_flight.is_some() || state.pending_merge.is_some() {
            return false;
        }
        if state
            .last_request_at
            .is_some_and(|last| last.elapsed() < std::time::Duration::from_millis(250))
        {
            return false;
        }
        let Some(cursor) = state.before_cursor.clone() else {
            return false;
        };
        cursor
    };
    let Some(request) =
        build_window_page_request(WindowPageCursor::Before(cursor), WINDOW_PAGE_LIMIT)
    else {
        return false;
    };
    let request_id = request.request_id().to_string();
    match claim_window_request(&request) {
        WindowClaim::Granted(_) => {
            let sent = atoms::HISTORY_WINDOW_TX
                .get()
                .is_some_and(|tx| tx.send(request).is_ok());
            if !sent {
                fail_window_request(&request_id, "history window channel is unavailable", false);
            }
            sent
        }
        WindowClaim::Merged => false,
        WindowClaim::Disabled => false,
        WindowClaim::Rejected(rejection) => {
            debug!(
                request_id = %request_id,
                "history_window_consumer: older page trigger rejected ({rejection:?})"
            );
            false
        }
    }
}

pub fn request_window_newer_page_if_due() -> bool {
    if !window_path_enabled() {
        return false;
    }
    let cursor = {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let state = handle.read();
        if state.is_degraded()
            || state.newer_exhausted
            || state.in_flight.is_some()
            || state.pending_merge.is_some()
        {
            return false;
        }
        if state
            .last_request_at
            .is_some_and(|last| last.elapsed() < std::time::Duration::from_millis(250))
        {
            return false;
        }
        let Some(cursor) = state.after_cursor.clone() else {
            return false;
        };
        cursor
    };
    let Some(request) =
        build_window_page_request(WindowPageCursor::After(cursor), WINDOW_PAGE_LIMIT)
    else {
        return false;
    };
    let request_id = request.request_id().to_string();
    match claim_window_request(&request) {
        WindowClaim::Granted(_) => {
            let sent = atoms::HISTORY_WINDOW_TX
                .get()
                .is_some_and(|tx| tx.send(request).is_ok());
            if !sent {
                fail_window_request(&request_id, "history window channel is unavailable", false);
            }
            sent
        }
        WindowClaim::Merged => false,
        WindowClaim::Disabled => false,
        WindowClaim::Rejected(rejection) => {
            debug!(
                request_id = %request_id,
                "history_window_consumer: newer page trigger rejected ({rejection:?})"
            );
            false
        }
    }
}

pub fn build_window_gap_request(
    window: &crate::kit::history_window::HistoryWindow,
) -> Option<WindowConsumerRequest> {
    match history_paging::window_gap_recovery(window)? {
        WindowGapRequest::Page { gap_id, cursor } => {
            build_window_page_request_for_gap(Some(gap_id), cursor, WINDOW_PAGE_LIMIT)
        }
        WindowGapRequest::Seek { gap_id, anchor } => build_window_seek_request_for_gap(
            Some(gap_id),
            anchor,
            WINDOW_PAGE_LIMIT,
            WINDOW_PAGE_LIMIT,
        ),
    }
}

/// 视口落在 gap（`WINDOW_VIEWPORT.anchor == None`）时的重读触发。
///
/// `direction` 来自本次滚动的落地方向：向上（更早）消费 `Page(Before)`，向下
/// （更晚）消费 `Page(After)`；`None`（方向未知）接受任一候选。seek 目标同时
/// 覆盖两侧。发送成功后只移除被消费的候选，另一方向留待反向滚动。
pub fn request_window_gap_if_due(direction: Option<GapSeekDirection>) -> bool {
    if !window_path_enabled() {
        return false;
    }
    let gap = {
        let handle = atoms::WINDOW_GAP_REQUEST.state();
        let requests = handle.read();
        let Some(gap) = requests
            .iter()
            .find(|request| request.matches_direction(direction))
            .cloned()
        else {
            return false;
        };
        gap
    };
    let request = match &gap {
        WindowGapRequest::Page { gap_id, cursor } => {
            build_window_page_request_for_gap(Some(*gap_id), cursor.clone(), WINDOW_PAGE_LIMIT)
        }
        WindowGapRequest::Seek { gap_id, anchor } => build_window_seek_request_for_gap(
            Some(*gap_id),
            anchor.clone(),
            WINDOW_PAGE_LIMIT,
            WINDOW_PAGE_LIMIT,
        ),
    };
    let Some(request) = request else {
        return false;
    };
    let request_id = request.request_id().to_string();
    match claim_window_request(&request) {
        WindowClaim::Granted(_) => {
            let sent = atoms::HISTORY_WINDOW_TX
                .get()
                .is_some_and(|tx| tx.send(request).is_ok());
            if sent {
                let handle = atoms::WINDOW_GAP_REQUEST.state();
                handle.write().retain(|candidate| candidate != &gap);
            } else {
                fail_window_request(&request_id, "history window gap channel unavailable", false);
            }
            sent
        }
        _ => false,
    }
}

pub fn enqueue_window_gap_request(window: &crate::kit::history_window::HistoryWindow) -> bool {
    let Some(request) = build_window_gap_request(window) else {
        return false;
    };
    atoms::HISTORY_WINDOW_TX
        .get()
        .is_some_and(|tx| tx.send(request).is_ok())
}

pub fn retry_window_open_if_due(now: Instant) -> Option<WindowConsumerRequest> {
    if !window_path_enabled() {
        return None;
    }
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    if state.in_flight.is_some() || state.pending_merge.is_some() {
        return None;
    }
    let retry_at = match &state.status {
        WindowPagingStatus::Degraded {
            retry_at: Some(retry_at),
            ..
        } => *retry_at,
        _ => return None,
    };
    if now < retry_at {
        return None;
    }
    let request_id = next_request_id(&mut state);
    Some(WindowConsumerRequest::Open {
        request_id,
        session_id: state.session_id.clone(),
        direction: WindowDirection::Before,
        limit: WINDOW_PAGE_LIMIT,
        byte_budget: WINDOW_DEFAULT_BYTE_BUDGET,
    })
}

pub fn request_window_retry_if_due() -> bool {
    let Some(request) = retry_window_open_if_due(Instant::now()) else {
        return false;
    };
    let request_id = request.request_id().to_string();
    match claim_window_request(&request) {
        WindowClaim::Granted(_) => {
            let sent = atoms::HISTORY_WINDOW_TX
                .get()
                .is_some_and(|tx| tx.send(request).is_ok());
            if !sent {
                fail_window_request(
                    &request_id,
                    "history window retry channel unavailable",
                    false,
                );
            }
            sent
        }
        _ => false,
    }
}
