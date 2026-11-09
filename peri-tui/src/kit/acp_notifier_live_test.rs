//! F2b：initial 前暂存的 live 更新在发布后按序重放。

use peri_acp_types::history_v2::{HISTORY_V2_VERSION, HistoryLimits};
use serde_json::{Value, json};
use serial_test::serial;

use super::*;
use crate::kit::atoms;
use crate::kit::history_v2::{HistoryV2State, initial, live};

struct LiveGuard;

impl Drop for LiveGuard {
    fn drop(&mut self) {
        atoms::HISTORY_V2_CAPABLE.set(false);
        atoms::HISTORY_V2_STATE.set(HistoryV2State::default());
        live::clear_all_pending();
    }
}

fn setup() -> LiveGuard {
    atoms::init_atoms();
    atoms::HISTORY_V2_CAPABLE.set(true);
    live::clear_all_pending();
    LiveGuard
}

fn admitted() -> HistoryV2State {
    let page = json!({
        "version": HISTORY_V2_VERSION,
        "kind": "window",
        "sessionId": "s1",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "historyRevision": "17",
        "committedTail": "19",
        "projectionVersion": 1,
        "readTicket": "ticket-1",
        "beforeCursor": "before-1",
        "afterCursor": "after-1",
        "items": [{
            "entryId": "s1:e1",
            "source": {"kind": "own"},
            "contentVersion": "1",
            "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "3"},
            "order": {"region": "own", "ordinal": "1"},
            "subItems": [{
                "kind": "text",
                "subItemId": "s1:e1:0",
                "role": "assistant",
                "text": "hello",
            }],
        }],
        "coverage": {"kind": "interval", "from": {"kind": "head"}, "to": {"kind": "tail"}},
        "progress": {"reason": "complete", "scannedEntries": 1, "consumedEntries": 1},
    });
    let response = json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": true,
                "runtimeGeneration": "runtime-9",
                "deliveryId": "delivery-51",
                "liveBoundary": {"streamId": "live-7", "afterSeq": "120"},
                "page": page,
            }
        }
    });
    initial::admit_load_response(
        &response,
        "s1",
        peri_acp_types::history_v2::DecimalU64::new(1),
        &HistoryLimits::default(),
    )
    .expect("admitted")
}

fn params(stream: &str, seq: u64, text: &str) -> Value {
    json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text},
            "messageId": "m1",
        },
        "_meta": {
            "peri.delivery": {
                "streamId": stream,
                "deliveryId": format!("{stream}:{seq}"),
                "eventSeq": seq.to_string(),
            }
        },
    })
}

fn buffered(state: &HistoryV2State, seq: u64, text: &str) {
    assert_eq!(
        live::gate_session_update(
            "s1",
            Some("agent_message_chunk"),
            &params("live-7", seq, text)
        ),
        live::LiveGate::Skip
    );
    assert!(live::is_pending("s1"), "state is {state:?}");
}

fn texts(events: &[AcpEventWithEpoch]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.event {
            AcpEventData::TextChunk(chunk) => Some(chunk.text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
#[serial]
fn replayed_updates_apply_once_in_sequence_order() {
    let _guard = setup();
    let state = admitted();
    atoms::HISTORY_V2_STATE.set(state.clone());
    live::begin_pending("s1");
    buffered(&state, 122, "b");
    buffered(&state, 121, "a");

    let events = replay_pending_initial(&state);
    assert_eq!(texts(&events), vec!["a", "b"]);
    let after = crate::kit::history_v2::snapshot();
    assert_eq!(
        after.window().unwrap().paging.live.last_applied_seq,
        Some(peri_acp_types::history_v2::DecimalU64::new(122))
    );
    assert!(!live::is_pending("s1"));
    assert!(live::take_pending("s1").is_empty());
}

#[test]
#[serial]
fn duplicate_buffered_notification_renders_once() {
    let _guard = setup();
    let state = admitted();
    atoms::HISTORY_V2_STATE.set(state.clone());
    live::begin_pending("s1");
    buffered(&state, 121, "a");
    buffered(&state, 121, "a");

    let events = replay_pending_initial(&state);
    assert_eq!(texts(&events), vec!["a"]);
    assert_eq!(
        crate::kit::history_v2::snapshot()
            .window()
            .unwrap()
            .paging
            .live
            .last_applied_seq,
        Some(peri_acp_types::history_v2::DecimalU64::new(121))
    );
}

/// 显式失效路径（旧窗口不可恢复）仍直接丢弃暂存；可恢复路径由
/// `replay_pending_into` 在客户端失败分支按序并回旧窗口。
#[test]
#[serial]
fn invalidated_initial_still_drops_pending_without_replay() {
    let _guard = setup();
    let state = admitted();
    atoms::HISTORY_V2_STATE.set(state.clone());
    live::begin_pending("s1");
    buffered(&state, 121, "a");
    live::clear_pending("s1");

    assert!(replay_pending_initial(&state).is_empty());
    assert!(live::take_pending("s1").is_empty());
}

/// 失败恢复：暂存按 seq 并入旧窗口后恰好渲染一次，高水位推进且无缺口。
#[test]
#[serial]
fn replay_pending_into_restores_old_window_once_in_order() {
    let _guard = setup();
    let state = admitted();
    atoms::HISTORY_V2_STATE.set(state.clone());
    live::begin_pending("s1");
    buffered(&state, 122, "b");
    buffered(&state, 121, "a");

    let mut restored = state.clone();
    let events = replay_pending_into(&mut restored);
    assert_eq!(texts(&events), vec!["a", "b"]);
    let window = restored.window().unwrap();
    assert_eq!(
        window.paging.live.last_applied_seq,
        Some(peri_acp_types::history_v2::DecimalU64::new(122))
    );
    assert_eq!(window.paging.live.gap, None);
    assert!(!window.paging.live.detached);
    assert!(!live::is_pending("s1"));

    assert!(
        replay_pending_into(&mut restored).is_empty(),
        "第二次恢复不得重复渲染"
    );
    assert_eq!(
        restored.window().unwrap().paging.live.last_applied_seq,
        Some(peri_acp_types::history_v2::DecimalU64::new(122))
    );
}

#[test]
#[serial]
fn stale_stream_is_dropped_and_does_not_advance_sequence() {
    let _guard = setup();
    let state = admitted();
    atoms::HISTORY_V2_STATE.set(state.clone());
    live::begin_pending("s1");
    assert_eq!(
        live::gate_session_update(
            "s1",
            Some("agent_message_chunk"),
            &params("live-old", 121, "stale")
        ),
        live::LiveGate::Skip
    );

    assert!(replay_pending_initial(&state).is_empty());
    assert_eq!(
        crate::kit::history_v2::snapshot()
            .window()
            .unwrap()
            .paging
            .live
            .last_applied_seq,
        Some(peri_acp_types::history_v2::DecimalU64::new(120))
    );
}

#[test]
#[serial]
fn publish_initial_drains_the_pending_buffer() {
    let _guard = setup();
    let state = admitted();
    live::begin_pending("s1");
    buffered(&state, 121, "a");
    buffered(&state, 122, "b");

    crate::kit::history_v2::publish_initial(state.clone());

    let after = crate::kit::history_v2::snapshot();
    assert_eq!(
        after.window().unwrap().paging.live.last_applied_seq,
        Some(peri_acp_types::history_v2::DecimalU64::new(122))
    );
    assert!(!live::is_pending("s1"));
}
