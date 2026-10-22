//! H5b 纯 reducer 测试：正文分块拼接、完成、失败占位与重试、迟到丢弃。

use peri_acp_types::history_v2::{DecimalU64, HISTORY_V2_VERSION, HistoryLimits, HistorySource};
use serde_json::{Value, json};

use super::body::{HistoryV2BodyLocator, HistoryV2BodySettlement, HistoryV2BodyState};
use super::initial::admit_load_response;
use super::paging::{HistoryV2CancelOutcome, HistoryV2RequestRejection};
use super::request::parse_body_result;
use super::{HistoryV2State, HistoryV2Window};

fn body_descriptor_with_total(total: &str) -> Value {
    json!({
        "bodyRef": "body-1",
        "representation": "display-text",
        "mediaType": "text/plain; charset=utf-8",
        "totalBytes": total,
        "chunkBytes": 5,
    })
}

fn initial_response() -> Value {
    initial_response_with_total("11")
}

fn initial_response_with_total(total: &str) -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": true,
                "runtimeGeneration": "runtime-1",
                "deliveryId": "delivery-1",
                "liveBoundary": {"streamId": "live-1", "afterSeq": "0"},
                "page": {
                    "version": HISTORY_V2_VERSION,
                    "kind": "window",
                    "sessionId": "s1",
                    "windowId": "w1",
                    "snapshotToken": "snapshot-1",
                    "historyRevision": "3",
                    "committedTail": null,
                    "projectionVersion": 1,
                    "readTicket": "ticket-1",
                    "beforeCursor": null,
                    "afterCursor": null,
                    "items": [{
                        "entryId": "e1",
                        "source": {"kind": "own"},
                        "contentVersion": "1",
                        "durability": {
                            "state": "sealed",
                            "origin": "live",
                            "finalItemSeq": "1",
                        },
                        "order": {"region": "own", "ordinal": "1"},
                        "subItems": [{
                            "kind": "opaque",
                            "subItemId": "body-0",
                            "body": body_descriptor_with_total(total),
                        }],
                    }],
                    "coverage": {
                        "kind": "interval",
                        "from": {
                            "kind": "position",
                            "region": "own",
                            "ordinal": "1",
                            "side": "before",
                        },
                        "to": {
                            "kind": "position",
                            "region": "own",
                            "ordinal": "2",
                            "side": "before",
                        },
                    },
                    "progress": {
                        "reason": "complete",
                        "scannedEntries": 1,
                        "consumedEntries": 1,
                    },
                },
            }
        }
    })
}

fn load() -> HistoryV2State {
    admit_load_response(&initial_response(), "s1", &HistoryLimits::default())
        .expect("initial admitted")
}

fn load_with_total(total: &str) -> HistoryV2State {
    admit_load_response(
        &initial_response_with_total(total),
        "s1",
        &HistoryLimits::default(),
    )
    .expect("initial admitted")
}

fn locator() -> HistoryV2BodyLocator {
    HistoryV2BodyLocator {
        source: HistorySource::Own,
        entry_id: "e1".into(),
        sub_item_id: "body-0".into(),
    }
}

fn state(window: &HistoryV2Window) -> &HistoryV2BodyState {
    window.paging.bodies.values().next().expect("body state")
}

fn chunk(data: &str, offset: &str, next: Option<&str>, complete: bool) -> Value {
    chunk_with_total(data, offset, next, complete, "11")
}

fn chunk_with_total(
    data: &str,
    offset: &str,
    next: Option<&str>,
    complete: bool,
    total: &str,
) -> Value {
    json!({
        "version": HISTORY_V2_VERSION,
        "kind": "body",
        "sessionId": "s1",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "bodyRef": "body-1",
        "contentVersion": "1",
        "representation": "display-text",
        "encoding": "base64",
        "offset": offset,
        "data": data,
        "nextOffset": next,
        "complete": complete,
        "totalBytes": total,
    })
}

#[test]
fn body_chunks_concatenate_in_offset_order_and_stop_when_complete() {
    let limits = HistoryLimits::default();
    let mut state_holder = load();
    let window = state_holder.window_mut().expect("window");

    let first = window
        .request_body(DecimalU64::new(2), &locator(), 5, &limits)
        .expect("first chunk request");
    assert_eq!(first.offset, DecimalU64::ZERO);
    let params = first.params();
    assert_eq!(params["action"], "body");
    assert_eq!(params["bodyRef"], "body-1");
    assert_eq!(params["offset"], "0");
    assert_eq!(params["maxBytes"], 5);

    let first_chunk =
        parse_body_result(&chunk("aGVsbG8=", "0", Some("5"), false)).expect("chunk decodes");
    assert_eq!(
        window.settle_body(&first, &first_chunk),
        HistoryV2BodySettlement::Accepted {
            received: 5,
            complete: false,
        }
    );
    assert_eq!(state(window).text(), Some("hello"));
    assert_eq!(state(window).next_offset, DecimalU64::new(5));
    assert!(
        window
            .paging
            .bodies
            .values()
            .all(|body| body.in_flight.is_none())
    );

    let second = window
        .request_body(DecimalU64::new(3), &locator(), 6, &limits)
        .expect("second chunk request");
    assert_eq!(second.offset, DecimalU64::new(5));
    let last_chunk = parse_body_result(&chunk("IHdvcmxk", "5", None, true)).expect("chunk decodes");
    assert_eq!(
        window.settle_body(&second, &last_chunk),
        HistoryV2BodySettlement::Accepted {
            received: 6,
            complete: true,
        }
    );
    let body = state(window);
    assert_eq!(body.text(), Some("hello world"));
    assert!(body.complete);
    assert_eq!(body.next_offset, DecimalU64::new(11));
    assert_eq!(window.paging.retained_body_bytes, 11);

    assert_eq!(
        window.request_body(DecimalU64::new(4), &locator(), 5, &limits),
        Err(HistoryV2RequestRejection::BodyComplete)
    );
}

#[test]
fn failed_chunk_keeps_placeholder_and_retry_resumes_from_confirmed_offset() {
    let limits = HistoryLimits::default();
    let mut state_holder = load();
    let window = state_holder.window_mut().expect("window");

    let request = window
        .request_body(DecimalU64::new(2), &locator(), 5, &limits)
        .expect("request");
    let wrong_offset =
        parse_body_result(&chunk("aGVsbG8=", "3", Some("8"), false)).expect("chunk decodes");
    assert!(matches!(
        window.settle_body(&request, &wrong_offset),
        HistoryV2BodySettlement::Rejected(_)
    ));
    let body = state(window);
    assert!(body.failed.is_some());
    assert!(body.in_flight.is_none());
    assert!(body.data.is_empty());
    assert_eq!(body.next_offset, DecimalU64::ZERO);
    assert_eq!(window.items.len(), 1, "失败仍保留条目占位，不删除历史行");

    let retry = window
        .request_body(DecimalU64::new(3), &locator(), 5, &limits)
        .expect("retry request");
    assert_eq!(retry.offset, DecimalU64::ZERO);
    assert!(state(window).failed.is_none(), "重试清除失败标记");
    let first_chunk =
        parse_body_result(&chunk("aGVsbG8=", "0", Some("5"), false)).expect("chunk decodes");
    assert!(matches!(
        window.settle_body(&retry, &first_chunk),
        HistoryV2BodySettlement::Accepted {
            received: 5,
            complete: false
        }
    ));

    let retry = window
        .request_body(DecimalU64::new(4), &locator(), 6, &limits)
        .expect("second retry request");
    assert_eq!(retry.offset, DecimalU64::new(5));
    let last_chunk = parse_body_result(&chunk("IHdvcmxk", "5", None, true)).expect("chunk decodes");
    assert!(matches!(
        window.settle_body(&retry, &last_chunk),
        HistoryV2BodySettlement::Accepted { complete: true, .. }
    ));
    assert_eq!(state(window).text(), Some("hello world"));
}

#[test]
fn late_body_response_does_not_write_into_a_newer_request() {
    let limits = HistoryLimits::default();
    let mut state_holder = load();
    let window = state_holder.window_mut().expect("window");

    let old_request = window
        .request_body(DecimalU64::new(2), &locator(), 5, &limits)
        .expect("old request");
    assert_eq!(
        window.cancel_request(DecimalU64::new(2)),
        HistoryV2CancelOutcome::Body
    );
    assert_eq!(
        window
            .request_body(DecimalU64::new(3), &locator(), 5, &limits)
            .expect("new request")
            .offset,
        DecimalU64::ZERO
    );

    let stale_chunk =
        parse_body_result(&chunk("aGVsbG8=", "0", Some("5"), false)).expect("chunk decodes");
    assert_eq!(
        window.settle_body(&old_request, &stale_chunk),
        HistoryV2BodySettlement::Stale
    );
    let body = state(window);
    assert!(body.data.is_empty(), "旧响应不得写入新请求");
    assert_eq!(
        body.in_flight.as_ref().map(|flight| flight.request_key),
        Some(DecimalU64::new(3)),
        "旧响应不得清掉新请求的在途标记"
    );
}

#[test]
fn body_budget_rejects_before_allocating_unbounded_state() {
    let limits = HistoryLimits::default();
    let mut state_holder = load();
    let window = state_holder.window_mut().expect("window");
    window.paging.retention.max_body_bytes = 4;

    assert_eq!(
        window.request_body(DecimalU64::new(2), &locator(), 5, &limits),
        Err(HistoryV2RequestRejection::BudgetExceeded)
    );
    assert!(window.paging.bodies.is_empty());
}

#[test]
fn first_chunk_adopts_authoritative_total_and_later_mismatch_is_rejected() {
    let limits = HistoryLimits::default();
    // 页面描述符给的是 canonical 预估（99），body 首块给出真实展示长度（11）。
    let mut state_holder = load_with_total("99");
    let window = state_holder.window_mut().expect("window");

    let first = window
        .request_body(DecimalU64::new(2), &locator(), 5, &limits)
        .expect("first request");
    let first_chunk =
        parse_body_result(&chunk("aGVsbG8=", "0", Some("5"), false)).expect("chunk decodes");
    assert!(matches!(
        window.settle_body(&first, &first_chunk),
        HistoryV2BodySettlement::Accepted {
            received: 5,
            complete: false
        }
    ));
    assert_eq!(state(window).total_bytes, 11, "首块修正权威总长");

    let second = window
        .request_body(DecimalU64::new(3), &locator(), 6, &limits)
        .expect("second request");
    let mismatched = parse_body_result(&chunk_with_total("IHdvcmxk", "5", None, true, "12"))
        .expect("chunk decodes");
    assert_eq!(
        window.settle_body(&second, &mismatched),
        HistoryV2BodySettlement::Rejected(super::body::HistoryV2BodyChunkRejection::InvalidLength),
        "后续块的 totalBytes 必须与首块一致"
    );
    assert_eq!(state(window).total_bytes, 11);
}

#[test]
fn first_chunk_adopts_authoritative_content_version() {
    let limits = HistoryLimits::default();
    let mut state_holder = load();
    let window = state_holder.window_mut().expect("window");

    let first = window
        .request_body(DecimalU64::new(2), &locator(), 5, &limits)
        .expect("first request");
    let mut first_chunk = chunk("aGVsbG8=", "0", Some("5"), false);
    first_chunk["contentVersion"] = json!("canonical-hash");
    let first_chunk = parse_body_result(&first_chunk).expect("chunk decodes");
    assert!(matches!(
        window.settle_body(&first, &first_chunk),
        HistoryV2BodySettlement::Accepted { .. }
    ));
    assert_eq!(state(window).content_version, "canonical-hash");

    let second = window
        .request_body(DecimalU64::new(3), &locator(), 6, &limits)
        .expect("second request");
    let mut other_chunk = chunk("IHdvcmxk", "5", None, true);
    other_chunk["contentVersion"] = json!("another-hash");
    let other_chunk = parse_body_result(&other_chunk).expect("chunk decodes");
    assert_eq!(
        window.settle_body(&second, &other_chunk),
        HistoryV2BodySettlement::Rejected(
            super::body::HistoryV2BodyChunkRejection::ContentVersionMismatch
        )
    );
    assert_eq!(state(window).content_version, "canonical-hash");
}
