//! H5a 纯 reducer 测试：initial wire → 唯一 V2 状态。

use peri_acp_types::history_v2::{HISTORY_V2_VERSION, HistoryLimits};
use serde_json::{Value, json};

use super::HistoryV2Status;
use super::initial::{InitialRejection, admit_load_response};

fn page() -> Value {
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
        "beforeCursor": null,
        "afterCursor": "after-1",
        "items": [{
            "entryId": "e1",
            "source": {"kind": "own"},
            "contentVersion": "1",
            "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "3"},
            "order": {"region": "own", "ordinal": "1"},
            "subItems": [{
                "kind": "text",
                "subItemId": "s1",
                "role": "user",
                "text": "hello",
            }],
        }],
        "coverage": {
            "kind": "interval",
            "from": {"kind": "head"},
            "to": {"kind": "tail"},
        },
        "progress": {"reason": "complete", "scannedEntries": 1, "consumedEntries": 1},
    })
}

fn initial(execution_ready: bool) -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": execution_ready,
                "runtimeGeneration": "runtime-9",
                "deliveryId": "delivery-51",
                "liveBoundary": {"streamId": "live-7", "afterSeq": "120"},
                "page": page(),
            }
        }
    })
}

#[test]
fn initial_maps_every_window_identity_into_one_state() {
    let state = admit_load_response(&initial(true), "s1", &HistoryLimits::default()).unwrap();
    assert_eq!(state.status, HistoryV2Status::Ready);
    assert!(state.is_ready());
    assert!(state.is_executable());
    assert_eq!(state.session_id.as_deref(), Some("s1"));
    assert_eq!(state.delivery_id.as_deref(), Some("delivery-51"));
    assert_eq!(
        state.live_boundary.as_ref().map(|b| b.stream_id.as_str()),
        Some("live-7")
    );
    let window = state.window().expect("window admitted");
    assert_eq!(window.window_id, "w1");
    assert_eq!(window.snapshot_token, "snapshot-1");
    assert_eq!(window.read_ticket, "ticket-1");
    assert_eq!(window.history_revision.get(), 17);
    assert_eq!(window.committed_tail.map(|tail| tail.get()), Some(19));
    assert_eq!(window.projection_version, 1);
    assert_eq!(window.before_cursor, None);
    assert_eq!(window.after_cursor.as_deref(), Some("after-1"));
    assert_eq!(window.items.len(), 1);
    assert_eq!(window.items[0].entry_id, "e1");
}

#[test]
fn execution_not_ready_keeps_history_readable_but_not_executable() {
    let state = admit_load_response(&initial(false), "s1", &HistoryLimits::default()).unwrap();
    assert!(state.is_ready(), "历史阅读不受 executionReady 影响");
    assert!(!state.is_executable(), "执行未就绪不得进入可执行态");
    assert!(state.window().is_some());
}

#[test]
fn missing_initial_is_explicit_rejection() {
    let error = admit_load_response(&json!({}), "s1", &HistoryLimits::default()).unwrap_err();
    assert_eq!(error.reason, InitialRejection::Missing);
    assert_eq!(error.reason.to_string(), "history_initial_required");
}

#[test]
fn v1_fields_in_v2_response_are_mixed_and_rejected() {
    let mut response = initial(true);
    response["nextCursor"] = json!("v1-cursor");
    let error = admit_load_response(&response, "s1", &HistoryLimits::default()).unwrap_err();
    assert_eq!(error.reason, InitialRejection::MixedFields);

    let mut response = initial(true);
    response["_meta"]["periLoadGeneration"] = json!("gen-1");
    let error = admit_load_response(&response, "s1", &HistoryLimits::default()).unwrap_err();
    assert_eq!(error.reason, InitialRejection::MixedFields);
}

#[test]
fn page_for_another_session_is_rejected() {
    let error =
        admit_load_response(&initial(true), "other", &HistoryLimits::default()).unwrap_err();
    assert_eq!(error.reason, InitialRejection::SessionMismatch);
    assert_eq!(error.reason.to_string(), "history_initial_session_mismatch");
}

#[test]
fn duplicate_entry_identity_is_rejected() {
    let mut response = initial(true);
    let duplicated = response["_meta"]["peri.history"]["page"]["items"][0].clone();
    response["_meta"]["peri.history"]["page"]["items"]
        .as_array_mut()
        .unwrap()
        .push(duplicated);
    let error = admit_load_response(&response, "s1", &HistoryLimits::default()).unwrap_err();
    assert_eq!(error.reason, InitialRejection::Invalid);
}
