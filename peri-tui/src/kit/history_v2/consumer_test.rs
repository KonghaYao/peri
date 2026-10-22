//! H5c：V2 请求触发/结算与失败恢复（纯 reducer，不发行 RPC）。

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
        atoms::HISTORY_V2.set(false);
        atoms::HISTORY_V2_CAPABLE.set(false);
        atoms::HISTORY_WINDOW_V1.set(false);
        atoms::HISTORY_WINDOW_CAPABLE.set(false);
        atoms::HISTORY_V2_STATE.set(HistoryV2State::default());
        reset_consumer_state();
    }
}

/// H5c 的触发测试共用一条队列；`OnceLock` 只能安装一次，接收端随模块存活到进程结束。
static TEST_RX: std::sync::Mutex<
    Option<tokio::sync::mpsc::UnboundedReceiver<HistoryV2ConsumerRequest>>,
> = std::sync::Mutex::new(None);

fn test_channel() {
    if atoms::HISTORY_V2_TX.get().is_some() {
        return;
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = atoms::HISTORY_V2_TX.set(tx);
    *TEST_RX.lock().unwrap() = Some(rx);
}

fn next_request() -> Option<HistoryV2ConsumerRequest> {
    TEST_RX.lock().unwrap().as_mut()?.try_recv().ok()
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
    admit_load_response(&initial_value(), "s1", &HistoryLimits::default()).expect("admitted")
}

fn page(value: Value) -> HistoryWindowPage {
    serde_json::from_value(value).expect("window page")
}

fn publish(state: &HistoryV2State) {
    atoms::HISTORY_V2_STATE.set(state.clone());
}

fn atomics_guard() -> AtomGuard {
    atoms::init_atoms();
    atoms::HISTORY_V2.set(true);
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
    let close = next_request().expect("close request enqueued");
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
    let open = next_request().expect("open request enqueued");
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

    atoms::HISTORY_V2.set(false);
    assert!(!request_open_latest("s1"), "beta 关闭时不发 open");
    assert!(next_request().is_none());

    atoms::HISTORY_V2.set(true);
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
    assert!(matches!(
        next_request(),
        Some(HistoryV2ConsumerRequest::Close { .. })
    ));
    let open = next_request().expect("open request enqueued");
    assert!(matches!(open, HistoryV2ConsumerRequest::Open { .. }));
    assert_eq!(open.params(&limits)["action"], json!("open"));
    // 原在途 open 标志不允许重复入队。
    assert!(!request_reopen(true));
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
fn request_keys_are_strictly_increasing_across_initial_and_pages() {
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
fn after_page_request_uses_after_cursor_and_v1_path_stays_disabled() {
    let _guard = atomics_guard();
    atoms::HISTORY_WINDOW_V1.set(true);
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    test_channel();
    drain_requests();
    let state = admitted();
    publish(&state);

    assert!(
        !crate::kit::history_page_consumer::window_path_enabled(),
        "V2 激活时 V1 窗口队列必须关闭"
    );
    assert!(request_newer_page_if_due());
    let Some(HistoryV2ConsumerRequest::Page(request)) = next_request() else {
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
    let Some(HistoryV2ConsumerRequest::Seek(request)) = next_request() else {
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
    let Some(HistoryV2ConsumerRequest::Close {
        session_id,
        window_id,
        snapshot_token,
        read_ticket,
        ..
    }) = next_request()
    else {
        panic!("重开必须先关闭旧窗口");
    };
    assert_eq!(session_id, "s1");
    assert_eq!(window_id, "w1");
    assert_eq!(snapshot_token, "snapshot-1");
    assert_eq!(read_ticket, "ticket-1");
    let Some(HistoryV2ConsumerRequest::Open { request_key, .. }) = next_request() else {
        panic!("失效后必须允许重新 open");
    };
    let mut reopened = page_json(Some("before-9"), json!([item("e9", 9, "reopened")]));
    reopened["windowId"] = json!("w2");
    reopened["snapshotToken"] = json!("snapshot-2");
    reopened["readTicket"] = json!("ticket-2");
    settle_open(&mut state, "s1", request_key, &page(reopened), &limits);

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
fn open_latest_enqueues_close_before_open_when_a_window_exists() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();
    publish(&admitted());

    assert!(request_open_latest("s1"));
    let Some(HistoryV2ConsumerRequest::Close {
        session_id,
        window_id,
        snapshot_token,
        read_ticket,
        ..
    }) = next_request()
    else {
        panic!("已有窗口时必须先入队 Close");
    };
    assert_eq!(session_id, "s1");
    assert_eq!(window_id, "w1");
    assert_eq!(snapshot_token, "snapshot-1");
    assert_eq!(read_ticket, "ticket-1");
    let Some(HistoryV2ConsumerRequest::Open { session_id, .. }) = next_request() else {
        panic!("Close 之后必须入队 Open");
    };
    assert_eq!(session_id, "s1");
    assert!(next_request().is_none());
}

#[test]
#[serial]
fn open_latest_without_a_window_enqueues_only_open() {
    let _guard = atomics_guard();
    test_channel();
    drain_requests();

    assert!(request_open_latest("s1"));
    let Some(HistoryV2ConsumerRequest::Open { session_id, .. }) = next_request() else {
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
    }) = next_request()
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
    let Some(HistoryV2ConsumerRequest::Open { request_key, .. }) = next_request() else {
        panic!("expected open request");
    };
    assert!(open_claim_is_some());

    // 响应解析失败/校验失败：归还 claim，窗口标记可重试。
    settle_open_failure(&mut state, "s1", request_key, "invalid_page_shape");
    assert!(!open_claim_is_some(), "解析失败必须复位在途标记");
    assert!(matches!(
        state.window().expect("window").paging.status,
        HistoryV2PagingStatus::Retryable { .. }
    ));

    assert!(
        request_open_latest("s1"),
        "上一次 open 失败后必须能发起新的 open"
    );
    // 已有旧窗口时先 best-effort Close，再进入新的 Open。
    assert!(matches!(
        next_request(),
        Some(HistoryV2ConsumerRequest::Close { .. })
    ));
    let Some(HistoryV2ConsumerRequest::Open { .. }) = next_request() else {
        panic!("expected a new open request");
    };
}
