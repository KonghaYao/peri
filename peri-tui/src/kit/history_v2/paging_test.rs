//! H5b 纯 reducer 测试：翻页、迟到丢弃、seek、coverage/gap、预算与 live 边界。

use peri_acp_types::history_v2::{
    DecimalU64, HISTORY_V2_VERSION, HistoryBoundary, HistoryDirection, HistoryEntryAnchor,
    HistoryLimits,
};
use serde_json::{Value, json};

use super::initial::admit_load_response;
use super::live::HistoryV2LiveAdmission;
use super::paging::{
    HistoryV2Anchor, HistoryV2CancelOutcome, HistoryV2DiscardReason, HistoryV2PageSettlement,
    HistoryV2PagingStatus, HistoryV2RequestRejection, HistoryV2SeekSettlement,
};
use super::request::parse_page_result;
use super::{HistoryV2State, HistoryV2Window};

fn entry(id: &str, ordinal: u64, text: &str) -> Value {
    json!({
        "entryId": id,
        "source": {"kind": "own"},
        "contentVersion": "1",
        "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "1"},
        "order": {"region": "own", "ordinal": ordinal.to_string()},
        "subItems": [{
            "kind": "text",
            "subItemId": format!("{id}-text"),
            "role": "user",
            "text": text,
        }],
    })
}

fn interval(from: u64, to: u64) -> Value {
    json!({
        "kind": "interval",
        "from": {
            "kind": "position",
            "region": "own",
            "ordinal": from.to_string(),
            "side": "before",
        },
        "to": {
            "kind": "position",
            "region": "own",
            "ordinal": to.to_string(),
            "side": "before",
        },
    })
}

fn page(
    items: Value,
    coverage: Value,
    before: Option<&str>,
    after: Option<&str>,
    window_id: &str,
    snapshot: &str,
) -> Value {
    json!({
        "version": HISTORY_V2_VERSION,
        "kind": "window",
        "sessionId": "s1",
        "windowId": window_id,
        "snapshotToken": snapshot,
        "historyRevision": "3",
        "committedTail": null,
        "projectionVersion": 1,
        "readTicket": "ticket-1",
        "beforeCursor": before,
        "afterCursor": after,
        "items": items,
        "coverage": coverage,
        "progress": {"reason": "complete", "scannedEntries": 1, "consumedEntries": 1},
    })
}

fn initial(items: Value, coverage: Value, before: Option<&str>, after: Option<&str>) -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": true,
                "runtimeGeneration": "runtime-1",
                "deliveryId": "delivery-1",
                "liveBoundary": {"streamId": "live-1", "afterSeq": "5"},
                "page": page(items, coverage, before, after, "w1", "snapshot-1"),
            }
        }
    })
}

fn load() -> HistoryV2State {
    admit_load_response(
        &initial(
            json!([entry("e2", 2, "two"), entry("e3", 3, "three")]),
            interval(2, 4),
            Some("b1"),
            Some("a1"),
        ),
        "s1",
        &HistoryLimits::default(),
    )
    .expect("initial admitted")
}

fn ids(window: &HistoryV2Window) -> Vec<&str> {
    window
        .items
        .iter()
        .map(|entry| entry.entry_id.as_str())
        .collect()
}

#[test]
fn before_and_after_paging_merge_without_duplicates() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");

    let before_request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("before page request");
    let params = before_request.params();
    assert_eq!(params["action"], "page");
    assert_eq!(params["cursor"], "b1");
    assert_eq!(params["direction"], "before");
    assert_eq!(params["requestKey"], "2");
    assert!(
        params.get("targetBytes").is_none(),
        "page 不允许临时扩大字节预算"
    );

    let before_page = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    match window.settle_page(&before_request, &before_page, &limits) {
        HistoryV2PageSettlement::Accepted { added, .. } => assert_eq!(added, 1),
        other => panic!("before page rejected: {other:?}"),
    }
    assert_eq!(ids(window), vec!["e1", "e2", "e3"]);
    assert_eq!(window.before_cursor.as_deref(), Some("b0"));
    assert_eq!(window.after_cursor.as_deref(), Some("a1"));
    assert!(window.paging.in_flight.is_none());

    let after_request = window
        .request_page(DecimalU64::new(3), HistoryDirection::After, 10, &limits)
        .expect("after page request");
    assert_eq!(after_request.params()["cursor"], "a1");
    let after_page = parse_page_result(&page(
        json!([entry("e4", 4, "four")]),
        interval(4, 5),
        Some("a1"),
        Some("a2"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    match window.settle_page(&after_request, &after_page, &limits) {
        HistoryV2PageSettlement::Accepted { added, .. } => assert_eq!(added, 1),
        other => panic!("after page rejected: {other:?}"),
    }

    assert_eq!(ids(window), vec!["e1", "e2", "e3", "e4"]);
    assert_eq!(window.after_cursor.as_deref(), Some("a2"));
    assert_eq!(window.coverage.len(), 1, "连续页应合并成一个 coverage 区间");
    assert!(window.gaps.is_empty());
    assert_eq!(window.retained_items, 4);
}

#[test]
fn opposite_direction_page_is_discarded_without_mutation() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");

    let request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("before page request");
    let wrong_side = parse_page_result(&page(
        json!([entry("e4", 4, "four")]),
        interval(4, 5),
        Some("b1"),
        Some("a2"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert_eq!(
        window.settle_page(&request, &wrong_side, &limits),
        HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongDirection)
    );
    assert_eq!(ids(window), vec!["e2", "e3"]);
    assert!(window.paging.in_flight.is_none());
    assert_eq!(window.paging.reserved_bytes, 0);

    let mut request = window
        .request_page(DecimalU64::new(3), HistoryDirection::Before, 10, &limits)
        .expect("retry after discard");
    request.direction = HistoryDirection::After;
    let valid_page = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert_eq!(
        window.settle_page(&request, &valid_page, &limits),
        HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongDirection)
    );
    assert_eq!(ids(window), vec!["e2", "e3"]);
    assert!(window.paging.in_flight.is_none());
}

#[test]
fn late_response_from_old_request_does_not_touch_new_state() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");

    let old_request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("old request");
    assert_eq!(
        window.fail_request(DecimalU64::new(2), "timeout", false),
        HistoryV2CancelOutcome::Page
    );
    assert!(matches!(
        window.paging.status,
        HistoryV2PagingStatus::Retryable { .. }
    ));

    let new_request = window
        .request_page(DecimalU64::new(3), HistoryDirection::Before, 10, &limits)
        .expect("new request");
    assert_eq!(
        window
            .paging
            .in_flight
            .as_ref()
            .map(|flight| flight.context.request_key),
        Some(DecimalU64::new(3))
    );

    let stale_page = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert_eq!(
        window.settle_page(&old_request, &stale_page, &limits),
        HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::LateRequest)
    );
    assert_eq!(
        window
            .paging
            .in_flight
            .as_ref()
            .map(|flight| flight.context.request_key),
        Some(DecimalU64::new(3)),
        "旧响应不得清除新请求的在途标记"
    );
    assert_eq!(ids(window), vec!["e2", "e3"]);

    let accepted = window.settle_page(&new_request, &stale_page, &limits);
    assert!(matches!(
        accepted,
        HistoryV2PageSettlement::Accepted { added: 1, .. }
    ));
    assert_eq!(ids(window), vec!["e1", "e2", "e3"]);
    assert!(window.paging.last_error.is_none());
}

#[test]
fn response_from_an_old_window_is_discarded_and_resets_in_flight() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");

    let request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("request");
    let old_window = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w-old",
        "snapshot-old",
    ))
    .expect("page decodes");
    assert_eq!(
        window.settle_page(&request, &old_window, &limits),
        HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::WrongWindow)
    );
    assert_eq!(ids(window), vec!["e2", "e3"]);
    assert!(window.paging.in_flight.is_none());
    assert_eq!(window.paging.reserved_bytes, 0);

    let retry = window
        .request_page(DecimalU64::new(3), HistoryDirection::Before, 10, &limits)
        .expect("retry uses the retained edge cursor");
    assert_eq!(retry.cursor, "b1");
}

#[test]
fn seek_replaces_payload_and_updates_anchor_and_home_cursor() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");
    let anchor = HistoryEntryAnchor {
        entry_id: "e2".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    };

    let request = window
        .request_seek(DecimalU64::new(2), &anchor, 10, 10, &limits)
        .expect("seek request");
    assert_eq!(request.params()["action"], "seek");
    assert_eq!(request.params()["anchor"]["entryId"], "e2");
    assert_eq!(window.paging.view_epoch, 2);

    let seek_page = parse_page_result(&page(
        json!([entry("e2", 2, "two"), entry("e3", 3, "three")]),
        interval(2, 4),
        Some("b2"),
        Some("a2"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert_eq!(
        window.settle_seek(&request, &seek_page, &limits),
        HistoryV2SeekSettlement::Accepted { retained: 2 }
    );
    assert_eq!(ids(window), vec!["e2", "e3"]);
    assert_eq!(window.before_cursor.as_deref(), Some("b2"));
    assert_eq!(window.after_cursor.as_deref(), Some("a2"));
    let home = window.paging.home.as_ref().expect("home");
    assert_eq!(
        home.anchor.as_ref().map(|anchor| anchor.entry_id.as_str()),
        Some("e2")
    );
    let anchor_state = window.paging.anchor.as_ref().expect("anchor state");
    assert!(anchor_state.retained);
    assert!(window.paging.bodies.is_empty(), "seek 丢弃旧窗口的正文状态");

    let missing = HistoryEntryAnchor {
        entry_id: "e9".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    };
    let request = window
        .request_seek(DecimalU64::new(3), &missing, 10, 10, &limits)
        .expect("seek request");
    let no_anchor = parse_page_result(&page(
        json!([entry("e2", 2, "two")]),
        interval(2, 3),
        Some("b2"),
        Some("a2"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert_eq!(
        window.settle_seek(&request, &no_anchor, &limits),
        HistoryV2SeekSettlement::AnchorUnavailable
    );
    assert_eq!(ids(window), vec!["e2", "e3"], "定位失败保留旧视图");
}

#[test]
fn coverage_gap_is_explicit_and_never_treated_as_continuous() {
    let limits = HistoryLimits::default();
    let mut state = admit_load_response(
        &initial(
            json!([entry("e3", 3, "three")]),
            interval(3, 4),
            Some("b1"),
            None,
        ),
        "s1",
        &limits,
    )
    .expect("initial admitted");
    let window = state.window_mut().expect("window");

    let request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("request");
    let sparse_page = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert!(matches!(
        window.settle_page(&request, &sparse_page, &limits),
        HistoryV2PageSettlement::Accepted { .. }
    ));

    assert_eq!(window.coverage.len(), 2, "不连续区间不得合并");
    assert_eq!(window.gaps.len(), 1);
    assert_eq!(
        window.gaps[0].from,
        HistoryBoundary::Position(peri_acp_types::history_v2::HistoryBoundaryPosition {
            region: "own".into(),
            ordinal: DecimalU64::new(2),
            side: peri_acp_types::history_v2::HistoryBoundarySide::Before,
        })
    );
    assert_eq!(
        window.gaps[0].to,
        HistoryBoundary::Position(peri_acp_types::history_v2::HistoryBoundaryPosition {
            region: "own".into(),
            ordinal: DecimalU64::new(3),
            side: peri_acp_types::history_v2::HistoryBoundarySide::Before,
        })
    );
    assert!(!window.empty_snapshot);
}

#[test]
fn retention_bounds_retained_items_and_keeps_reload_cursor() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");
    window.paging.retention.max_items = 2;

    let request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("request");
    let before_page = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert!(matches!(
        window.settle_page(&request, &before_page, &limits),
        HistoryV2PageSettlement::Accepted { .. }
    ));

    assert_eq!(window.items.len(), 1, "对侧整页淘汰，保留窗口不随翻页增长");
    assert_eq!(window.retained_items, 1);
    assert_eq!(window.segments.len(), 1);
    assert_eq!(
        window.after_cursor.as_deref(),
        Some("b1"),
        "被淘汰一侧仍保留可重读的服务端游标"
    );
}

#[test]
fn anchor_becomes_unavailable_after_its_entry_is_evicted() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");
    window.set_anchor(HistoryV2Anchor {
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "e3".into(),
        sub_item_id: None,
        content_offset: 7,
    });
    assert!(window.paging.anchor.as_ref().expect("anchor").retained);

    window.paging.retention.max_items = 1;
    let request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("request");
    let before_page = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert!(matches!(
        window.settle_page(&request, &before_page, &limits),
        HistoryV2PageSettlement::Accepted { .. }
    ));

    let anchor = window
        .paging
        .anchor
        .as_ref()
        .expect("anchor retained identity");
    assert!(!anchor.retained, "锚点条目被淘汰后必须显式失效");
    assert_eq!(anchor.anchor.entry_id, "e3");
}

#[test]
fn live_events_are_gated_by_boundary_and_record_gaps() {
    let mut state = load();
    let window = state.window_mut().expect("window");
    assert_eq!(
        window.accept_live_event("live-1", DecimalU64::new(4)),
        HistoryV2LiveAdmission::BeforeBoundary
    );
    assert_eq!(
        window.accept_live_event("live-1", DecimalU64::new(5)),
        HistoryV2LiveAdmission::Duplicate
    );
    assert_eq!(
        window.accept_live_event("live-1", DecimalU64::new(6)),
        HistoryV2LiveAdmission::Accepted {
            seq: DecimalU64::new(6)
        }
    );
    assert_eq!(
        window.accept_live_event("live-1", DecimalU64::new(8)),
        HistoryV2LiveAdmission::GapRecorded {
            expected: DecimalU64::new(7),
            received: DecimalU64::new(8),
        }
    );
    assert_eq!(
        window.accept_live_event("live-1", DecimalU64::new(9)),
        HistoryV2LiveAdmission::Detached
    );
    let live = &window.paging.live;
    assert!(live.detached);
    assert_eq!(
        live.last_applied_seq.map(|seq| seq.get()),
        Some(6),
        "缺口响应不得推进已应用水位"
    );
    assert_eq!(
        live.gap
            .as_ref()
            .map(|gap| (gap.expected.get(), gap.received.get())),
        Some((7, 8))
    );
}

#[test]
fn seek_preempts_page_and_old_page_cannot_clear_the_new_request() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");
    let old_page_request = window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("page request");
    let anchor = HistoryEntryAnchor {
        entry_id: "e2".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    };
    let seek_request = window
        .request_seek(DecimalU64::new(3), &anchor, 10, 10, &limits)
        .expect("seek preempts the page");

    assert!(matches!(
        window.paging.in_flight.as_ref().map(|flight| &flight.kind),
        Some(super::paging::HistoryV2InFlightKind::Seek { .. })
    ));
    assert_eq!(
        window.paging.reserved_bytes,
        usize::try_from(limits.target_page_bytes).unwrap(),
        "page 预留在 seek 抢占后只保留一份"
    );
    let old_page = parse_page_result(&page(
        json!([entry("e1", 1, "one")]),
        interval(1, 2),
        Some("b0"),
        Some("b1"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert_eq!(
        window.settle_page(&old_page_request, &old_page, &limits),
        HistoryV2PageSettlement::Discarded(HistoryV2DiscardReason::LateRequest)
    );
    assert_eq!(
        window
            .paging
            .in_flight
            .as_ref()
            .map(|flight| flight.context.request_key),
        Some(DecimalU64::new(3)),
        "旧 page 不得清掉新 seek 的在途标记"
    );

    let seek_page = parse_page_result(&page(
        json!([entry("e2", 2, "two")]),
        interval(2, 3),
        Some("b2"),
        Some("a2"),
        "w1",
        "snapshot-1",
    ))
    .expect("page decodes");
    assert!(matches!(
        window.settle_seek(&seek_request, &seek_page, &limits),
        HistoryV2SeekSettlement::Accepted { .. }
    ));
    assert_eq!(ids(window), vec!["e2"]);
}

#[test]
fn cancel_and_timeout_release_in_flight_and_allow_retry() {
    let limits = HistoryLimits::default();
    let mut state = load();
    let window = state.window_mut().expect("window");

    window
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits)
        .expect("request");
    assert_eq!(
        window.cancel_request(DecimalU64::new(2)),
        HistoryV2CancelOutcome::Page
    );
    assert!(window.paging.in_flight.is_none());
    assert_eq!(window.paging.reserved_bytes, 0);
    assert_eq!(
        window.request_page(DecimalU64::new(2), HistoryDirection::Before, 10, &limits),
        Err(HistoryV2RequestRejection::ReusedRequestKey),
        "取消后不得复用旧 requestKey 重发旧请求"
    );

    window
        .request_page(DecimalU64::new(3), HistoryDirection::Before, 10, &limits)
        .expect("retry after cancel");
    assert_eq!(
        window.fail_request(DecimalU64::new(3), "timeout", false),
        HistoryV2CancelOutcome::Page
    );
    assert!(window.paging.in_flight.is_none());
    assert_eq!(window.paging.reserved_bytes, 0);
    assert!(matches!(
        window.paging.status,
        HistoryV2PagingStatus::Retryable { .. }
    ));

    window
        .request_page(DecimalU64::new(4), HistoryDirection::Before, 10, &limits)
        .expect("retry after timeout");

    assert_eq!(
        window.fail_request(DecimalU64::new(4), "snapshot_invalidated", true),
        HistoryV2CancelOutcome::Page
    );
    assert!(matches!(
        window.paging.status,
        HistoryV2PagingStatus::ReopenRequired { .. }
    ));
    assert_eq!(
        window.request_page(DecimalU64::new(5), HistoryDirection::Before, 10, &limits),
        Err(HistoryV2RequestRejection::ReopenRequired)
    );
}
