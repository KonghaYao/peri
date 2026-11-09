//! F2b：实时投递解析、接纳门与 initial 前暂存。

use peri_acp_types::history_v2::{HISTORY_V2_VERSION, HistoryLimits};
use serde_json::{Value, json};
use serial_test::serial;

use super::*;

struct LiveGuard;

impl Drop for LiveGuard {
    fn drop(&mut self) {
        atoms::HISTORY_V2_CAPABLE.set(false);
        atoms::HISTORY_V2_STATE.set(super::super::HistoryV2State::default());
        clear_all_pending();
    }
}

fn setup() -> LiveGuard {
    atoms::init_atoms();
    atoms::HISTORY_V2_CAPABLE.set(true);
    clear_all_pending();
    LiveGuard
}

fn page(items: Value) -> Value {
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
        "beforeCursor": "before-1",
        "afterCursor": "after-1",
        "items": items,
        "coverage": {"kind": "interval", "from": {"kind": "head"}, "to": {"kind": "tail"}},
        "progress": {"reason": "complete", "scannedEntries": 1, "consumedEntries": 1},
    })
}

fn admitted() -> super::super::HistoryV2State {
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
                "page": page(json!([{
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
                }])),
            }
        }
    });
    super::super::initial::admit_load_response(
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

fn meta(stream: &str, seq: u64) -> DeliveryMeta {
    DeliveryMeta {
        stream_id: stream.to_owned(),
        delivery_id: format!("{stream}:{seq}"),
        seq: DecimalU64::new(seq),
    }
}

#[test]
#[serial]
fn parses_delivery_meta_and_treats_bad_shapes_as_absent() {
    let _guard = setup();
    let parsed = DeliveryMeta::parse(&params("live-7", 121, "x")).expect("valid meta");
    assert_eq!(parsed.stream_id, "live-7");
    assert_eq!(parsed.seq, DecimalU64::new(121));

    assert!(DeliveryMeta::parse(&json!({"update": {}})).is_none());
    assert!(DeliveryMeta::parse(&json!({"_meta": {"peri.delivery": {}}})).is_none());
    // eventSeq 必须是十进制字符串；数字形状视为无 meta，不 panic。
    assert!(
        DeliveryMeta::parse(&json!({
            "_meta": {"peri.delivery": {
                "streamId": "s", "deliveryId": "d", "eventSeq": 1
            }}
        }))
        .is_none()
    );
    assert!(
        DeliveryMeta::parse(&json!({
            "_meta": {"peri.delivery": {
                "streamId": "", "deliveryId": "d", "eventSeq": "1"
            }}
        }))
        .is_none()
    );
}

#[test]
#[serial]
fn admission_accepts_only_contiguous_transcript_sequence() {
    let _guard = setup();
    atoms::HISTORY_V2_STATE.set(admitted());

    assert_eq!(
        admit_live_delivery("s1", &meta("live-7", 121)),
        HistoryV2LiveAdmission::Accepted {
            seq: DecimalU64::new(121)
        }
    );
    assert_eq!(
        admit_live_delivery("s1", &meta("live-7", 121)),
        HistoryV2LiveAdmission::Duplicate
    );
    assert_eq!(
        admit_live_delivery("s1", &meta("live-7", 120)),
        HistoryV2LiveAdmission::BeforeBoundary
    );
    assert_eq!(
        admit_live_delivery("s1", &meta("live-8", 122)),
        HistoryV2LiveAdmission::WrongStream
    );

    let gap = admit_live_delivery("s1", &meta("live-7", 124));
    assert!(matches!(gap, HistoryV2LiveAdmission::GapRecorded { .. }));
    assert_eq!(
        admit_live_delivery("s1", &meta("live-7", 125)),
        HistoryV2LiveAdmission::Detached
    );
    let state = super::super::snapshot();
    assert!(state.window().unwrap().paging.live.detached);
}

#[test]
#[serial]
fn gate_renders_v1_without_meta_and_skips_non_transcript() {
    let _guard = setup();
    let plain = json!({"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk"}});
    assert_eq!(
        gate_session_update("s1", Some("agent_message_chunk"), &plain),
        LiveGate::Render
    );
    let commands = json!({
        "sessionId": "s1",
        "update": {"sessionUpdate": "available_commands_update"},
        "_meta": {"peri.delivery": {"streamId": "s", "deliveryId": "d", "eventSeq": "1"}},
    });
    assert_eq!(
        gate_session_update("s1", Some("available_commands_update"), &commands),
        LiveGate::Render
    );
}

#[test]
#[serial]
fn pending_buffer_dedupes_sorts_and_drops_oldest_on_overflow() {
    let _guard = setup();
    begin_pending("s1");
    assert!(is_pending("s1"));
    assert_eq!(
        gate_session_update(
            "s1",
            Some("agent_message_chunk"),
            &params("live-7", 122, "b")
        ),
        LiveGate::Skip
    );
    assert_eq!(
        gate_session_update(
            "s1",
            Some("agent_message_chunk"),
            &params("live-7", 121, "a")
        ),
        LiveGate::Skip
    );
    // 同一 stream+seq 的重复通知只暂存一次。
    assert_eq!(
        gate_session_update(
            "s1",
            Some("agent_message_chunk"),
            &params("live-7", 121, "a")
        ),
        LiveGate::Skip
    );
    let taken = take_pending("s1");
    assert_eq!(taken.len(), 2);
    assert_eq!(taken[0].0.seq, DecimalU64::new(121));
    assert_eq!(taken[1].0.seq, DecimalU64::new(122));
    assert!(!is_pending("s1"));

    begin_pending("s1");
    for seq in 200..265 {
        gate_session_update(
            "s1",
            Some("agent_message_chunk"),
            &params("live-7", seq, "chunk"),
        );
    }
    let taken = take_pending("s1");
    assert_eq!(taken.len(), MAX_PENDING_UPDATES);
    assert_eq!(taken[0].0.seq, DecimalU64::new(201));
}

#[test]
#[serial]
fn pending_clear_is_per_session_and_global_reset_drops_everything() {
    let _guard = setup();
    begin_pending("s1");
    begin_pending("s2");
    gate_session_update("s1", Some("agent_message_chunk"), &params("live-a", 1, "a"));
    gate_session_update("s2", Some("agent_message_chunk"), &params("live-b", 5, "b"));
    clear_pending("s1");
    assert!(take_pending("s1").is_empty());
    assert_eq!(take_pending("s2").len(), 1);

    begin_pending("s1");
    clear_all_pending();
    assert!(take_pending("s1").is_empty());
    assert!(!is_pending("s2"));
}
