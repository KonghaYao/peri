//! H5c：V2 请求触发/结算与失败恢复（纯 reducer，不发行 RPC）。

use peri_acp::transport::AcpTransport;
use peri_acp_types::history_v2::{
    HISTORY_V2_VERSION, HistoryDirection, HistoryEntryAnchor, HistoryErrorReason, HistoryLimits,
    HistoryRecovery, HistoryWindowPage,
};
use serde_json::{Value, json};
use serial_test::serial;

use super::*;
use crate::kit::history_v2::HistoryV2State;
use crate::kit::history_v2::error::HistoryV2WireFailure;
use crate::kit::history_v2::initial::admit_load_response;
use crate::kit::history_v2::paging::{HistoryV2PagingStatus, HistoryV2RequestRejection};
use peri_acp_types::history_v2::DecimalU64;

struct AtomGuard;

impl Drop for AtomGuard {
    fn drop(&mut self) {
        atoms::HISTORY_V2_CAPABLE.set(false);
        atoms::HISTORY_V2_STATE.set(HistoryV2State::default());
        *atoms::HISTORY_V2_TX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        reset_consumer_state();
    }
}

/// H5c 的触发测试共用一条队列；每个串行用例安装自己的出站调度通道。
static TEST_RX: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<OutboundCommand>>> =
    std::sync::Mutex::new(None);

fn test_channel() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    *TEST_RX.lock().unwrap() = Some(rx);
}

fn next_request() -> Option<OutboundCommand> {
    TEST_RX.lock().unwrap().as_mut()?.try_recv().ok()
}

fn next_window_request() -> Option<HistoryV2ConsumerRequest> {
    match next_request()? {
        OutboundCommand::Request(request) => Some(request),
        OutboundCommand::Body { .. } | OutboundCommand::Load { .. } => {
            panic!("expected a window request")
        }
    }
}

fn drain_requests() {
    while next_request().is_some() {}
}

fn page_json(before_cursor: Option<&str>, items: Value) -> Value {
    json!({
        "version": HISTORY_V2_VERSION,
        "kind": "window",
        "sessionId": "s1",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "historyRevision": "17",
        "committedTail": "19",
        "projectionVersion": 1,
        "readTicket": "ticket-1",
        "beforeCursor": before_cursor,
        "afterCursor": "after-1",
        "items": items,
        "coverage": {
            "kind": "interval",
            "from": {"kind": "head"},
            "to": {"kind": "tail"},
        },
        "progress": {"reason": "complete", "scannedEntries": 1, "consumedEntries": 1},
    })
}

fn item(id: &str, ordinal: u64, text: &str) -> Value {
    json!({
        "entryId": format!("s1:{id}"),
        "source": {"kind": "own"},
        "contentVersion": "1",
        "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "3"},
        "order": {"region": "own", "ordinal": ordinal.to_string()},
        "subItems": [{
            "kind": "text",
            "subItemId": format!("s1:{id}:0"),
            "role": "assistant",
            "text": text,
        }],
    })
}

fn initial_value() -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": true,
                "runtimeGeneration": "runtime-9",
                "deliveryId": "delivery-51",
                "liveBoundary": {"streamId": "live-7", "afterSeq": "120"},
                "page": page_json(Some("before-1"), json!([item("e2", 5, "newest")])),
            }
        }
    })
}

fn admitted() -> HistoryV2State {
    admit_load_response(
        &initial_value(),
        "s1",
        DecimalU64::new(1),
        &HistoryLimits::default(),
    )
    .expect("admitted")
}

fn page(value: Value) -> HistoryWindowPage {
    serde_json::from_value(value).expect("window page")
}

fn publish(state: &HistoryV2State) {
    atoms::HISTORY_V2_STATE.set(state.clone());
}

fn atomics_guard() -> AtomGuard {
    atoms::init_atoms();
    atoms::HISTORY_V2_CAPABLE.set(true);
    atoms::HISTORY_V2_LIMITS.set(HistoryLimits::default());
    atoms::HISTORY_V2_STATE.set(HistoryV2State::default());
    // 手工构造的 initial 状态回显 requestKey=1；触发侧从同一位置继续分配。
    seed_request_key(DecimalU64::new(1));
    AtomGuard
}

#[test]
#[serial]
fn close_request_uses_window_identity_without_faking_closed_state() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    let state = admitted();
    publish(&state);

    assert!(request_close_current());
    let close = next_window_request().expect("close request enqueued");
    let HistoryV2ConsumerRequest::Close {
        session_id,
        window_id,
        snapshot_token,
        read_ticket,
        ..
    } = &close
    else {
        panic!("expected close request, got {close:?}");
    };
    assert_eq!(session_id, "s1");
    assert_eq!(window_id, "w1");
    assert_eq!(snapshot_token, "snapshot-1");
    assert_eq!(read_ticket, "ticket-1");
    let params = close.params(&HistoryLimits::default());
    assert_eq!(params["action"], json!("close"));
    assert_eq!(params["windowId"], json!("w1"));
    assert_eq!(params["snapshotToken"], json!("snapshot-1"));
    assert_eq!(params["readTicket"], json!("ticket-1"));
    // best-effort：本地不把窗口伪造成已关闭。
    assert!(crate::kit::history_v2::snapshot().is_ready());
}

#[test]
#[serial]
fn close_for_other_session_is_not_enqueued() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    publish(&admitted());

    assert!(!request_close_for_session("s2"));
    assert!(next_request().is_none(), "不得关闭其他会话的窗口");
}

#[test]
#[serial]
fn new_session_open_latest_builds_first_window_and_page_is_usable() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    atoms::HISTORY_V2_STATE.set(HistoryV2State::default());

    assert!(request_open_latest("s2"), "新会话必须请求首个窗口");
    let open = next_window_request().expect("open request enqueued");
    let HistoryV2ConsumerRequest::Open {
        session_id,
        request_key,
        anchor,
    } = &open
    else {
        panic!("expected open request, got {open:?}");
    };
    assert_eq!(session_id, "s2");
    assert!(anchor.is_none(), "新会话从 latest 建立首窗");
    assert_eq!(
        open.params(&HistoryLimits::default())["action"],
        json!("open")
    );

    let limits = HistoryLimits::default();
    let mut state = HistoryV2State::default();
    let mut page_value = page_json(Some("before-1"), json!([item("e2", 5, "newest")]));
    page_value["sessionId"] = json!("s2");
    settle_open(&mut state, "s2", *request_key, &page(page_value), &limits);
    let mut state = crate::kit::history_v2::snapshot();
    assert!(state.is_ready(), "open 响应建立 Ready 状态");
    let window = state.window().expect("window");
    assert_eq!(window.session_id, "s2");
    assert!(window.window_id == "w1");

    let page_request = state
        .window_mut()
        .expect("window")
        .request_page(
            DecimalU64::new(request_key.get() + 1),
            HistoryDirection::Before,
            50,
            &limits,
        )
        .expect("page request usable after first open");
    assert_eq!(page_request.cursor, "before-1");
}

#[test]
#[serial]
fn open_latest_is_not_enqueued_without_active_v2() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();

    atoms::HISTORY_V2_CAPABLE.set(false);
    assert!(!request_open_latest("s1"), "未协商时不发 open");
    assert!(next_request().is_none());
}

#[test]
#[serial]
fn older_page_request_uses_window_cursor_and_settles_once() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    let request = state
        .window_mut()
        .expect("window")
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .expect("page request");
    assert_eq!(request.cursor, "before-1");
    assert_eq!(request.params()["action"], json!("page"));
    assert!(request.params().get("nextCursor").is_none());
    publish(&state);

    assert!(settle_page(
        &mut state,
        &request,
        &page(page_json(None, json!([item("e1", 3, "older")]))),
        &limits,
    ));
    let window = state.window().expect("window");
    assert_eq!(window.items.len(), 2);
    assert_eq!(window.before_cursor, None);
    assert!(window.paging.in_flight.is_none());
    assert_eq!(
        atoms::HISTORY_V2_STATE
            .state()
            .read()
            .window
            .as_ref()
            .map(|w| w.items.len()),
        Some(2)
    );
}

#[test]
#[serial]
fn seek_advances_epoch_and_replaces_retained_window() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    publish(&state);
    let anchor = HistoryEntryAnchor {
        entry_id: "s1:e1".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    };
    let before_epoch = state.window().expect("window").paging.view_epoch;
    let request = state
        .window_mut()
        .expect("window")
        .request_seek(DecimalU64::new(3), &anchor, 25, 25, &limits)
        .expect("seek request");
    assert_eq!(request.params()["action"], json!("seek"));
    // seek 在发起时推进导航代次（旧响应此后按 epoch 丢弃），结算不再二次推进。
    assert_eq!(request.context.view_epoch, before_epoch + 1);
    assert!(settle_seek(
        &mut state,
        &request,
        &page(page_json(
            Some("before-9"),
            json!([item("e1", 3, "target")])
        )),
        &limits,
    ));
    let window = state.window().expect("window");
    assert_eq!(window.paging.view_epoch, before_epoch + 1);
    assert_eq!(window.items.len(), 1);
    assert!(window.paging.anchor.as_ref().is_some_and(|a| a.retained));
}

#[test]
#[serial]
fn window_expiry_requires_reopen_and_reopen_enqueues_open() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    let request = state
        .window_mut()
        .expect("window")
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .expect("page request");
    publish(&state);
    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::WindowExpired),
        recovery: Some(HistoryRecovery::Reopen),
    };
    assert!(!apply_failure(
        &mut state,
        request.context.identity(),
        &failure
    ));
    let window = state.window().expect("window");
    assert!(matches!(
        window.paging.status,
        HistoryV2PagingStatus::ReopenRequired { .. }
    ));
    assert!(window.paging.in_flight.is_none());
    publish(&state);

    assert!(request_reopen(false), "窗口失效后必须允许重新 open");
    let Some(HistoryV2ConsumerRequest::Open { request_key, .. }) = next_window_request() else {
        panic!("重开必须先发送 Open");
    };
    assert!(next_request().is_none(), "新窗口被接纳前不得关闭旧窗口");

    // 模拟消费者：Open 响应通过准入后才释放旧窗口身份。
    release_open_for("s1", request_key);
    let mut reopened_page = page_json(Some("before-9"), json!([item("e9", 9, "reopened")]));
    reopened_page["windowId"] = json!("w2");
    reopened_page["snapshotToken"] = json!("snapshot-2");
    reopened_page["readTicket"] = json!("ticket-2");
    let old = settle_open(&mut state, "s1", request_key, &page(reopened_page), &limits)
        .expect("旧窗口必须在接纳后返回");
    assert!(request_close_window(old));
    let Some(HistoryV2ConsumerRequest::Close {
        window_id,
        snapshot_token,
        ..
    }) = next_window_request()
    else {
        panic!("接纳成功后必须关闭旧窗口");
    };
    assert_eq!(window_id, "w1");
    assert_eq!(snapshot_token, "snapshot-1");
}

#[test]
#[serial]
fn operation_in_progress_keeps_original_claim_and_no_retry_key() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    let request = state
        .window_mut()
        .expect("window")
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .expect("page request");
    publish(&state);
    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::OperationInProgress),
        recovery: None,
    };
    assert!(!apply_failure(
        &mut state,
        request.context.identity(),
        &failure
    ));
    let window = state.window().expect("window");
    assert!(window.paging.in_flight.is_some(), "不得释放在途认领");
    assert_eq!(window.paging.status, HistoryV2PagingStatus::Ready);
    // 触发侧看到在途后不会分配第二个 key。
    assert_eq!(
        state.window_mut().expect("window").request_page(
            DecimalU64::new(3),
            HistoryDirection::Before,
            50,
            &limits
        ),
        Err(HistoryV2RequestRejection::Busy)
    );
    apply_transport_failure(&mut state, request.context.identity(), "timeout");
    assert!(state.window().expect("window").paging.in_flight.is_none());
    assert!(matches!(
        state.window().expect("window").paging.status,
        HistoryV2PagingStatus::Retryable { .. }
    ));
}

#[test]
#[serial]
fn discard_only_failure_keeps_page_and_clears_claim() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    let request = state
        .window_mut()
        .expect("window")
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .expect("page request");
    publish(&state);
    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::InvalidCursor),
        recovery: None,
    };
    assert!(apply_failure(
        &mut state,
        request.context.identity(),
        &failure
    ));
    let window = state.window().expect("window");
    assert!(window.paging.in_flight.is_none());
    assert_eq!(window.items.len(), 1, "丢弃请求不得改动已保留页");
    assert_eq!(window.paging.status, HistoryV2PagingStatus::Ready);
}

#[test]
#[serial]
fn seed_only_raises_the_allocator_high_watermark() {
    let _guard = atomics_guard();
    reset_request_key_for_test();
    let initial = next_request_key();
    assert_eq!(initial.get(), 1, "initial 与页请求共用同一分配器");
    let page = next_request_key();
    assert_eq!(page.get(), 2);
    // 服务端回显只能把高水位推高，不能把分配器拨回旧键。
    seed_request_key(initial);
    assert_eq!(next_request_key().get(), 3);
}

#[test]
#[serial]
fn open_params_carry_no_window_identity() {
    let request = HistoryV2ConsumerRequest::Open {
        session_id: "s1".into(),
        request_key: DecimalU64::new(9),
        anchor: Some(HistoryEntryAnchor {
            entry_id: "s1:e1".into(),
            source: peri_acp_types::history_v2::HistorySource::Own,
        }),
    };
    let params = request.params(&HistoryLimits::default());
    assert_eq!(params["action"], json!("open"));
    assert_eq!(params["anchor"]["kind"], json!("entry"));
    assert!(params.get("windowId").is_none());
    assert!(params.get("snapshotToken").is_none());
}

#[test]
#[serial]
fn after_page_request_uses_after_cursor() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    let state = admitted();
    publish(&state);

    assert!(request_newer_page_if_due());
    let Some(HistoryV2ConsumerRequest::Page(request)) = next_window_request() else {
        panic!("after 方向必须进入 V2 reducer 的页请求");
    };
    assert_eq!(request.direction, HistoryDirection::After);
    assert_eq!(request.cursor, "after-1");
    assert_eq!(request.params()["direction"], json!("after"));
    assert!(
        !request_newer_page_if_due(),
        "同一窗口至多一个在途页请求，不得重复分配"
    );
}

#[test]
#[serial]
fn gap_trigger_seeks_the_nearest_retained_entry() {
    use peri_acp_types::history_v2::{
        HistoryBoundary, HistoryBoundaryPosition, HistoryBoundarySide, HistorySource,
    };

    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    let mut state = admitted();
    state
        .window_mut()
        .expect("window")
        .gaps
        .push(crate::kit::history_v2::paging::HistoryV2Gap {
            from: HistoryBoundary::Head,
            to: HistoryBoundary::Position(HistoryBoundaryPosition {
                region: "own".into(),
                ordinal: DecimalU64::new(2),
                side: HistoryBoundarySide::After,
            }),
        });
    publish(&state);

    assert!(request_gap_if_due());
    let Some(HistoryV2ConsumerRequest::Seek(request)) = next_window_request() else {
        panic!("gap 必须由同一 reducer 以 seek 重建");
    };
    assert_eq!(request.anchor.entry_id, "s1:e2");
    assert_eq!(request.anchor.source, HistorySource::Own);
}

#[test]
#[serial]
fn snapshot_invalidated_resets_claim_and_reopen_replaces_the_window() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    let request = state
        .window_mut()
        .expect("window")
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .expect("page request");
    publish(&state);
    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::SnapshotInvalidated),
        recovery: Some(HistoryRecovery::Reopen),
    };
    assert!(!apply_failure(
        &mut state,
        request.context.identity(),
        &failure
    ));
    assert!(state.window().expect("window").paging.in_flight.is_none());
    publish(&state);

    assert!(request_reopen(false));
    let Some(HistoryV2ConsumerRequest::Open { request_key, .. }) = next_window_request() else {
        panic!("失效后必须允许重新 open");
    };
    assert!(next_request().is_none(), "Open 未被接纳前不得关闭旧窗口");
    let mut reopened = page_json(Some("before-9"), json!([item("e9", 9, "reopened")]));
    reopened["windowId"] = json!("w2");
    reopened["snapshotToken"] = json!("snapshot-2");
    reopened["readTicket"] = json!("ticket-2");
    release_open_for("s1", request_key);
    let old = settle_open(&mut state, "s1", request_key, &page(reopened), &limits)
        .expect("接纳后返回旧窗口");
    assert_eq!(old.window_id, "w1");
    assert_eq!(old.snapshot_token, "snapshot-1");
    assert!(request_close_window(old));
    let Some(HistoryV2ConsumerRequest::Close {
        session_id,
        window_id,
        snapshot_token,
        read_ticket,
        ..
    }) = next_window_request()
    else {
        panic!("接纳成功后必须关闭旧窗口");
    };
    assert_eq!(session_id, "s1");
    assert_eq!(window_id, "w1");
    assert_eq!(snapshot_token, "snapshot-1");
    assert_eq!(read_ticket, "ticket-1");

    let reopened = crate::kit::history_v2::snapshot();
    assert!(reopened.is_ready());
    let window = reopened.window().expect("reopened window");
    assert_eq!(window.window_id, "w2");
    assert_eq!(window.snapshot_token, "snapshot-2");
    assert!(window.paging.status.is_ready());
    assert!(window.paging.in_flight.is_none());
    assert!(window.paging.view_epoch > 1, "重开推进导航代次");
    assert_eq!(window.items.len(), 1);
}

#[test]
#[serial]
fn open_latest_sends_open_then_closes_old_window_only_after_admission() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    publish(&state);

    assert!(request_open_latest("s1"));
    let Some(HistoryV2ConsumerRequest::Open {
        session_id,
        request_key: open_key,
        ..
    }) = next_window_request()
    else {
        panic!("已有窗口时也必须先发送 Open");
    };
    assert_eq!(session_id, "s1");
    assert!(next_request().is_none(), "新窗口接纳前不得关闭旧窗口");

    release_open_for("s1", open_key);
    let mut next_page = page_json(Some("before-2"), json!([item("e9", 9, "new")]));
    next_page["windowId"] = json!("w2");
    next_page["snapshotToken"] = json!("snapshot-2");
    next_page["readTicket"] = json!("ticket-2");
    let old = settle_open(&mut state, "s1", open_key, &page(next_page), &limits)
        .expect("接纳成功后返回旧窗口");
    assert!(request_close_window(old));
    let Some(HistoryV2ConsumerRequest::Close {
        session_id,
        window_id,
        request_key: close_key,
        ..
    }) = next_window_request()
    else {
        panic!("接纳成功后必须关闭旧窗口");
    };
    assert_eq!(session_id, "s1");
    assert_eq!(window_id, "w1");
    assert!(close_key > open_key, "旧窗口的 Close key 必须晚于 Open key");
    assert!(next_request().is_none());
}

#[test]
#[serial]
fn open_latest_without_a_window_enqueues_only_open() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();

    assert!(request_open_latest("s1"));
    let Some(HistoryV2ConsumerRequest::Open { session_id, .. }) = next_window_request() else {
        panic!("没有窗口时只入队 Open");
    };
    assert_eq!(session_id, "s1");
    assert!(next_request().is_none());
}

fn s2_page() -> HistoryWindowPage {
    let mut value = page_json(Some("before-2"), json!([item("e7", 7, "s2")]));
    value["sessionId"] = json!("s2");
    value["windowId"] = json!("w2");
    value["snapshotToken"] = json!("snapshot-2");
    value["readTicket"] = json!("ticket-2");
    page(value)
}

#[test]
#[serial]
fn late_failure_from_previous_session_does_not_touch_new_window() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    let mut s1 = admitted();
    let request = s1
        .window_mut()
        .expect("s1 window")
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .expect("s1 page request");
    let identity = request.context.identity();

    // 切到 s2：新会话窗口已经建立。
    let mut s2 = HistoryV2State::default();
    settle_open(&mut s2, "s2", DecimalU64::new(9), &s2_page(), &limits);
    let before = s2.clone();

    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::SnapshotInvalidated),
        recovery: Some(HistoryRecovery::Reopen),
    };
    assert!(!apply_failure(&mut s2, identity, &failure));
    assert_eq!(s2, before, "旧会话失败不得修改新会话窗口");

    // 迟到的成功 open 同样不能替换新会话窗口。
    settle_open(
        &mut s2,
        "s1",
        DecimalU64::new(10),
        &page(page_json(Some("before-1"), json!([item("e2", 5, "s1")]))),
        &limits,
    );
    assert_eq!(s2, before, "旧会话 open 响应不得替换新会话窗口");
}

#[test]
#[serial]
fn superseded_failure_does_not_release_the_new_in_flight_claim() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    let mut state = admitted();
    let request = state
        .window_mut()
        .expect("window")
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .expect("page request");
    let stale_identity = HistoryV2RequestIdentity {
        session_id: "s1",
        window_id: Some("w1"),
        request_key: DecimalU64::new(3),
    };
    let before = state.clone();

    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::SnapshotInvalidated),
        recovery: Some(HistoryRecovery::Reopen),
    };
    assert!(!apply_failure(&mut state, stale_identity, &failure));
    assert_eq!(state, before, "被替代请求的失败不得释放当前在途认领");
    assert!(state.window().expect("window").paging.in_flight.is_some());
    assert_eq!(
        state
            .window()
            .expect("window")
            .paging
            .in_flight
            .as_ref()
            .unwrap()
            .context
            .request_key,
        request.context.request_key
    );
}

#[test]
#[serial]
fn late_open_failure_keeps_the_new_session_claim() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();

    assert!(request_open_latest("s2"), "B 的 open 必须能发起");
    let Some(HistoryV2ConsumerRequest::Open {
        request_key: b_key, ..
    }) = next_window_request()
    else {
        panic!("expected B open request");
    };
    assert!(open_claim_is_some());

    let mut state = HistoryV2State::default();
    settle_open_failure(&mut state, "s1", DecimalU64::new(1), "invalid_page_shape");

    assert!(
        open_claim_is_some(),
        "旧会话的 open 失败不得清掉新会话的在途标记"
    );
    assert!(!request_open_latest("s3"), "B 的 open 仍在途，不得重复分配");
    assert_eq!(state, HistoryV2State::default());

    release_open_for("s2", b_key);
    assert!(!open_claim_is_some());
}

#[test]
#[serial]
fn open_failure_releases_the_claim_so_the_next_open_can_start() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    let mut state = admitted();

    assert!(request_open_latest("s1"));
    let Some(HistoryV2ConsumerRequest::Open { request_key, .. }) = next_window_request() else {
        panic!("expected open request");
    };
    assert!(open_claim_is_some());

    // 响应解析失败/校验失败：归还 claim；Open 失败一律回到重开态。
    settle_open_failure(&mut state, "s1", request_key, "invalid_page_shape");
    assert!(!open_claim_is_some(), "解析失败必须复位在途标记");
    assert!(matches!(
        state.window().expect("window").paging.status,
        HistoryV2PagingStatus::ReopenRequired { .. }
    ));
    assert!(next_request().is_none(), "Open 失败时不得关闭旧窗口");

    assert!(
        request_open_latest("s1"),
        "上一次 open 失败后必须能发起新的 open"
    );
    let Some(HistoryV2ConsumerRequest::Open { .. }) = next_window_request() else {
        panic!("expected a new open request");
    };
}
// ---------------------------------------------------------------------------
// 真实出站顺序：记录型 mock client。
// ---------------------------------------------------------------------------

fn wire_key(value: &Value) -> u64 {
    value
        .as_str()
        .expect("DecimalU64 must serialize as a string")
        .parse()
        .expect("canonical decimal key")
}

fn initial_response_for(
    session: &str,
    window: &str,
    before_cursor: &str,
    request_key: &str,
) -> Value {
    let mut value = initial_value();
    value["_meta"]["peri.history"]["requestKey"] = json!(request_key);
    let page = &mut value["_meta"]["peri.history"]["page"];
    page["sessionId"] = json!(session);
    page["windowId"] = json!(window);
    page["snapshotToken"] = json!(format!("snapshot-{window}"));
    page["readTicket"] = json!(format!("ticket-{window}"));
    page["beforeCursor"] = json!(before_cursor);
    value
}

fn page_response_for(session: &str, window: &str, before_cursor: &str, items: Value) -> Value {
    let mut value = page_json(Some(before_cursor), items);
    value["sessionId"] = json!(session);
    value["windowId"] = json!(window);
    value["snapshotToken"] = json!(format!("snapshot-{window}"));
    value["readTicket"] = json!(format!("ticket-{window}"));
    value
}

async fn next_wire_request(
    server: &peri_acp::transport::mpsc::MpscServerTransport,
) -> (peri_acp::transport::types::RequestId, String, Value) {
    use peri_acp::transport::types::IncomingMessage;
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), server.recv())
        .await
        .expect("expected a wire request")
        .expect("transport must stay open");
    match message {
        IncomingMessage::Request { id, method, params } => (id, method, params),
        other => panic!("expected request, got {other:?}"),
    }
}

async fn respond_wire(
    server: &peri_acp::transport::mpsc::MpscServerTransport,
    id: peri_acp::transport::types::RequestId,
    result: Value,
) {
    server.send_response(id, Ok(result)).await.unwrap();
}

/// 所有 V2 RPC 的线上 key 必须严格递增，且入队顺序 = 发送顺序。
///
/// 顺序：initial → Close(旧窗口) → page → body → open → Close(被替换窗口) → seek。
/// body 若绕过队列，会在 page 响应前到达服务端；这里用超时断言它没有。
#[tokio::test]
#[serial]
async fn wire_requests_follow_enqueue_order_across_all_v2_rpcs() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    reset_request_key_for_test();

    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = crate::acp_client::client::AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, limits.clone());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = spawn_history_v2_consumer(client.clone(), rx, shutdown.clone());

    // 已发布的旧窗口：initial 成功前必须保持可用，成功后才会 Close。
    publish(&admitted());

    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("s1", "/tmp", None).await });

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, "session/load");
    let initial_key = wire_key(&params["_meta"]["peri.history"]["requestKey"]);
    assert_eq!(initial_key, 1, "initial 是第一个操作");
    respond_wire(
        &server,
        id,
        initial_response_for("s1", "w2", "before-1", "1"),
    )
    .await;
    load.await.unwrap().unwrap();
    let loaded_state = crate::kit::history_v2::snapshot();
    assert!(loaded_state.is_ready());
    assert_eq!(
        loaded_state
            .window()
            .map(|window| window.window_id.as_str()),
        Some("w2"),
        "initial 成功后替换为候选窗口"
    );
    assert_eq!(loaded_state.delivery_id.as_deref(), Some("delivery-51"));

    // initial 接纳成功后才关闭旧窗口，且 key 晚于 initial。
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));
    let close_old_key = wire_key(&params["requestKey"]);
    assert!(close_old_key > initial_key);
    assert_eq!(params["windowId"], json!("w1"));
    respond_wire(&server, id, json!({})).await;

    // page 先入队，body 紧随其后；body 在 page 响应前不得上线。
    assert!(request_older_page_if_due());
    let state = crate::kit::history_v2::snapshot();
    let window = state.window().expect("initial window");
    let binding = crate::kit::history_detail_binding::HistoryV2BodyBinding {
        session_id: "s1".into(),
        window_id: window.window_id.clone(),
        snapshot_token: window.snapshot_token.clone(),
        read_ticket: window.read_ticket.clone(),
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "s1:e2".into(),
        sub_item_id: "s1:e2:0".into(),
        body_ref: "body-1".into(),
        representation: peri_acp_types::history_v2::HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".into(),
    };
    let body_receiver = match submit_body(&binding, 0, 128).expect("scheduler installed") {
        crate::kit::history_v2::outbound::Submit::Queued(receiver) => receiver,
        crate::kit::history_v2::outbound::Submit::Direct(_) => {
            panic!("body must be queued, not sent directly")
        }
    };
    assert!(request_open_latest("s1"));

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("page"));
    let page_key = wire_key(&params["requestKey"]);
    assert!(page_key > close_old_key);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), server.recv())
            .await
            .is_err(),
        "body/open 不得在 page 响应前越过队列"
    );
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w2", "before-2", json!([item("e0", 1, "older")])),
    )
    .await;

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("body"));
    let body_key = wire_key(&params["requestKey"]);
    assert!(body_key > page_key, "body 必须排在 page 之后");
    respond_wire(&server, id, json!({})).await;
    assert!(body_receiver.await.unwrap().is_ok());

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("open"));
    let open_key = wire_key(&params["requestKey"]);
    assert!(open_key > body_key);
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w3", "before-3", json!([item("e9", 9, "open")])),
    )
    .await;

    // Open 接纳成功后才为被替换窗口入队 Close。
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));
    let close_replaced_key = wire_key(&params["requestKey"]);
    assert!(close_replaced_key > open_key, "Close key 必须晚于 Open key");
    assert_eq!(params["windowId"], json!("w2"));
    respond_wire(&server, id, json!({})).await;

    let anchor = HistoryEntryAnchor {
        entry_id: "s1:e9".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    };
    assert!(request_seek_if_due(anchor));
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("seek"));
    let seek_key = wire_key(&params["requestKey"]);
    assert!(seek_key > close_replaced_key);
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w3", "before-3", json!([item("e9", 9, "open")])),
    )
    .await;

    shutdown.cancel();
    consumer.abort();
    client.close();
}

/// reset/会话切换后，旧 session 的 Open 成功响应不得在空状态上重建窗口。
#[tokio::test]
#[serial]
async fn late_open_success_after_reset_is_discarded() {
    let _guard = atomics_guard();
    reset_request_key_for_test();

    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = crate::acp_client::client::AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, HistoryLimits::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = spawn_history_v2_consumer(client.clone(), rx, shutdown.clone());

    publish(&admitted());
    assert!(request_reopen(true));
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("open"));
    assert_eq!(params["sessionId"], json!("s1"));

    // 会话边界：清空窗口并作废在途认领。
    crate::kit::history_v2::reset_state();
    reset_consumer_state();

    // 迟到的成功响应携带旧窗口页面；它没有仍然有效的 claim，必须被丢弃。
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w1", "before-1", json!([item("e2", 5, "late")])),
    )
    .await;

    // 用新的 s2 Open 作为“旧响应已处理”的屏障。
    assert!(request_open_latest("s2"));
    let (_, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("open"));
    assert_eq!(params["sessionId"], json!("s2"));
    assert_eq!(
        crate::kit::history_v2::snapshot(),
        HistoryV2State::default(),
        "旧会话的 Open 成功响应不得复活窗口"
    );

    shutdown.cancel();
    consumer.abort();
    client.close();
}

/// 调度器已关闭时，page/seek 已登记的在途与预留必须回滚。
#[test]
#[serial]
fn page_and_seek_send_failure_roll_back_in_flight_state() {
    let _guard = atomics_guard();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    publish(&admitted());

    assert!(!request_older_page_if_due(), "队列关闭时不得报告已入队");
    {
        let state = crate::kit::history_v2::snapshot();
        let paging = &state.window().expect("window").paging;
        assert!(paging.in_flight.is_none(), "page 在途必须回滚");
        assert_eq!(paging.reserved_bytes, 0, "page 预留必须回滚");
    }

    assert!(!request_seek_if_due(HistoryEntryAnchor {
        entry_id: "s1:e2".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    }));
    {
        let state = crate::kit::history_v2::snapshot();
        let paging = &state.window().expect("window").paging;
        assert!(paging.in_flight.is_none(), "seek 在途必须回滚");
        assert_eq!(paging.reserved_bytes, 0, "seek 预留必须回滚");
    }
}
