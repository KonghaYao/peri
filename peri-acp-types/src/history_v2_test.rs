//! History V2 wire 契约测试：规范整数、五个 action、页面/条目、正文、协商与错误。

use serde_json::{json, Map, Value};

use super::*;

/// 正文块形状与 opaque 占位子项（拆出以维持单文件 ≤1000 行）。
#[path = "history_v2_body_test.rs"]
mod body_and_opaque_tests;

fn limits() -> HistoryLimits {
    HistoryLimits::default()
}

fn latest_anchor() -> Value {
    json!({ "kind": "latest" })
}

fn entry_anchor() -> Value {
    json!({
        "kind": "entry",
        "entryId": "e1",
        "source": { "kind": "own" }
    })
}

fn open_wire(
    request_key: &str,
    anchor: Value,
    before: u32,
    after: u32,
    target_bytes: u64,
) -> Value {
    json!({
        "version": 2,
        "action": "open",
        "sessionId": "s1",
        "requestKey": request_key,
        "anchor": anchor,
        "before": before,
        "after": after,
        "targetBytes": target_bytes
    })
}

fn page_wire(limit: u32) -> Value {
    json!({
        "version": 2,
        "action": "page",
        "sessionId": "s1",
        "requestKey": "2",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "cursor": "cursor-1",
        "direction": "before",
        "limit": limit
    })
}

fn seek_wire(before: u32, after: u32) -> Value {
    json!({
        "version": 2,
        "action": "seek",
        "sessionId": "s1",
        "requestKey": "3",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "anchor": entry_anchor(),
        "before": before,
        "after": after
    })
}

fn body_wire(max_bytes: u64) -> Value {
    json!({
        "version": 2,
        "action": "body",
        "sessionId": "s1",
        "requestKey": "4",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "bodyRef": "body-1",
        "offset": "0",
        "maxBytes": max_bytes
    })
}

fn close_wire() -> Value {
    json!({
        "version": 2,
        "action": "close",
        "sessionId": "s1",
        "requestKey": "5",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1"
    })
}

fn decode_open(value: &Value) -> HistoryOpenRequest {
    match HistoryRequest::decode(value).unwrap() {
        HistoryRequest::Open(request) => request,
        other => panic!("expected open, got {other:?}"),
    }
}

fn decode_page(value: &Value) -> HistoryPageRequest {
    match HistoryRequest::decode(value).unwrap() {
        HistoryRequest::Page(request) => request,
        other => panic!("expected page, got {other:?}"),
    }
}

fn with_extra(mut value: Value) -> Value {
    value
        .as_object_mut()
        .expect("wire object")
        .insert("legacy".to_owned(), json!(true));
    value
}

fn descriptor() -> HistoryBodyDescriptor {
    HistoryBodyDescriptor {
        body_ref: "body-1".to_owned(),
        representation: HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".to_owned(),
        total_bytes: DecimalU64::new(4096),
        chunk_bytes: 65536,
    }
}

fn text_item(id: &str, text: &str) -> HistorySubItem {
    HistorySubItem::Text(HistoryTextSubItem {
        sub_item_id: id.to_owned(),
        role: "assistant".to_owned(),
        text: Some(text.to_owned()),
        body: None,
    })
}

fn entry(entry_id: &str, ordinal: u64, sub_items: Vec<HistorySubItem>) -> HistoryEntry {
    HistoryEntry {
        entry_id: entry_id.to_owned(),
        source: HistorySource::Own,
        content_version: "v-content-1".to_owned(),
        durability: HistoryDurability {
            state: HistoryDurabilityState::Sealed,
            origin: HistoryOrigin::Live,
            final_item_seq: Some(DecimalU64::new(9)),
        },
        order: HistoryOrder {
            region: "1".to_owned(),
            ordinal: DecimalU64::new(ordinal),
        },
        sub_items,
    }
}

fn window_page() -> HistoryWindowPage {
    HistoryWindowPage {
        version: 2,
        kind: HistoryWindowKind::Window,
        session_id: "s1".to_owned(),
        window_id: "w1".to_owned(),
        snapshot_token: "snapshot-1".to_owned(),
        history_revision: DecimalU64::new(17),
        committed_tail: Some(DecimalU64::new(42)),
        projection_version: 2,
        read_ticket: "ticket-1".to_owned(),
        before_cursor: Some("before-1".to_owned()),
        after_cursor: Some("after-1".to_owned()),
        items: vec![
            entry("e42", 42, vec![text_item("text-0", "已完成修改。")]),
            entry(
                "e43",
                43,
                vec![HistorySubItem::ToolResult(HistoryToolResultSubItem {
                    sub_item_id: "tool-result-0".to_owned(),
                    invocation_key: "invocation-1".to_owned(),
                    status: "completed".to_owned(),
                    output: None,
                    body: Some(descriptor()),
                })],
            ),
        ],
        coverage: HistoryCoverage::Interval(HistoryCoverageInterval {
            from: HistoryBoundary::Position(HistoryBoundaryPosition {
                region: "1".to_owned(),
                ordinal: DecimalU64::new(41),
                side: HistoryBoundarySide::Before,
            }),
            to: HistoryBoundary::Tail,
        }),
        progress: HistoryProgress {
            reason: HistoryProgressReason::Complete,
            scanned_entries: 2,
            consumed_entries: 2,
        },
        expires_at: Some("2026-10-22T12:00:00Z".to_owned()),
    }
}

#[test]
fn decimal_u64_round_trips_zero_and_max() {
    let zero = DecimalU64::new(0);
    assert_eq!(zero.to_string(), "0");
    assert_eq!("0".parse::<DecimalU64>().unwrap(), zero);
    assert_eq!(serde_json::to_string(&zero).unwrap(), "\"0\"");

    let max = DecimalU64::new(u64::MAX);
    let encoded = serde_json::to_string(&max).unwrap();
    assert_eq!(encoded, "\"18446744073709551615\"");
    assert_eq!(serde_json::from_str::<DecimalU64>(&encoded).unwrap(), max);
    assert_eq!(DecimalU64::try_from("42").unwrap().get(), 42);
}

#[test]
fn decimal_u64_rejects_non_canonical_and_overflow() {
    for value in [
        "01",
        "+1",
        "-1",
        "",
        "1.0",
        "18446744073709551616",
        "000000000000000000000",
        "999999999999999999999",
    ] {
        assert!(
            value.parse::<DecimalU64>().is_err(),
            "{value} must be rejected"
        );
    }
}

#[test]
fn decimal_u64_rejects_json_numbers() {
    assert!(serde_json::from_str::<DecimalU64>("1").is_err());
    assert!(serde_json::from_str::<DecimalU64>("null").is_err());
}

#[test]
fn request_key_zero_is_rejected() {
    let mut request = decode_open(&open_wire("0", latest_anchor(), 50, 0, 524288));
    assert!(request.validate(&limits()).is_err());
    request.request_key = DecimalU64::new(1);
    request.validate(&limits()).unwrap();
}

#[test]
fn all_five_actions_decode() {
    let open = HistoryRequest::decode(&open_wire("1", latest_anchor(), 50, 0, 524288)).unwrap();
    assert_eq!(open.action(), "open");
    assert!(matches!(open, HistoryRequest::Open(_)));

    let page = HistoryRequest::decode(&page_wire(50)).unwrap();
    assert_eq!(page.action(), "page");
    assert!(matches!(page, HistoryRequest::Page(_)));

    let seek = HistoryRequest::decode(&seek_wire(25, 24)).unwrap();
    assert_eq!(seek.action(), "seek");
    assert!(matches!(seek, HistoryRequest::Seek(_)));

    let body = HistoryRequest::decode(&body_wire(65536)).unwrap();
    assert_eq!(body.action(), "body");
    assert!(matches!(body, HistoryRequest::Body(_)));

    let close = HistoryRequest::decode(&close_wire()).unwrap();
    assert_eq!(close.action(), "close");
    assert!(matches!(close, HistoryRequest::Close(_)));
}

#[test]
fn unknown_or_missing_action_is_rejected() {
    assert!(HistoryRequest::decode(&json!({ "action": "rewind", "version": 2 })).is_err());
    assert!(HistoryRequest::decode(&json!({ "version": 2 })).is_err());
    assert!(HistoryRequest::decode(&json!(["open"])).is_err());
}

#[test]
fn request_variants_reject_unknown_fields() {
    for value in [
        open_wire("1", latest_anchor(), 50, 0, 524288),
        page_wire(50),
        seek_wire(25, 24),
        body_wire(65536),
        close_wire(),
    ] {
        assert!(HistoryRequest::decode(&with_extra(value)).is_err());
    }
}

#[test]
fn initial_decode_accepts_canonical_request() {
    let value = json!({
        "version": 2,
        "action": "initial",
        "requestKey": "1",
        "limit": 50,
        "targetBytes": 524288
    });
    let request = HistoryInitialRequest::decode(&value).unwrap();
    assert_eq!(request.request_key, DecimalU64::new(1));
    request.validate(&limits()).unwrap();
}

#[test]
fn initial_decode_rejects_legacy_top_level_fields() {
    for field in ["cursor", "loadGeneration", "history"] {
        let mut object = json!({
            "version": 2,
            "action": "initial",
            "requestKey": "1",
            "limit": 50,
            "targetBytes": 524288
        })
        .as_object()
        .unwrap()
        .clone();
        object.insert(field.to_owned(), json!({"old": true}));
        assert!(
            HistoryInitialRequest::decode(&Value::Object(object)).is_err(),
            "{field} must be rejected"
        );
    }
}

#[test]
fn initial_action_must_be_initial() {
    let not_initial = json!({
        "version": 2,
        "action": "open",
        "requestKey": "1",
        "limit": 50,
        "targetBytes": 524288
    });
    assert!(HistoryInitialRequest::decode(&not_initial).is_err());
    assert!(HistoryInitialRequest::decode(&json!({
        "version": 2,
        "requestKey": "1",
        "limit": 50,
        "targetBytes": 524288
    }))
    .is_err());
}

#[test]
fn open_latest_count_and_target_bounds() {
    let mut request = decode_open(&open_wire("1", latest_anchor(), 50, 0, 524288));
    request.validate(&limits()).unwrap();

    request.before = 0;
    assert!(request.validate(&limits()).is_err());
    request.before = 101;
    assert!(request.validate(&limits()).is_err());
    request.before = 100;
    request.validate(&limits()).unwrap();

    request.before = 50;
    request.after = 1;
    assert!(request.validate(&limits()).is_err());

    let mut target = decode_open(&open_wire("1", latest_anchor(), 50, 0, 65535));
    assert!(target.validate(&limits()).is_err());
    target.target_bytes = 65536;
    target.validate(&limits()).unwrap();
    target.target_bytes = 524289;
    assert!(target.validate(&limits()).is_err());
}

#[test]
fn open_entry_around_count_bounds() {
    let ok = decode_open(&open_wire("1", entry_anchor(), 50, 49, 524288));
    ok.validate(&limits()).unwrap();

    let too_many = decode_open(&open_wire("1", entry_anchor(), 60, 40, 524288));
    assert!(too_many.validate(&limits()).is_err());

    let too_many_before = decode_open(&open_wire("1", entry_anchor(), 100, 0, 524288));
    assert!(too_many_before.validate(&limits()).is_err());
}

#[test]
fn page_limit_bounds_and_direction() {
    for limit in [0, 101] {
        let request = decode_page(&page_wire(limit));
        assert!(
            request.validate(&limits()).is_err(),
            "limit {limit} must be rejected"
        );
    }
    for limit in [1, 100] {
        decode_page(&page_wire(limit)).validate(&limits()).unwrap();
    }

    let mut invalid_direction = page_wire(50);
    invalid_direction["direction"] = json!("sideways");
    assert!(HistoryRequest::decode(&invalid_direction).is_err());
}

#[test]
fn body_max_bytes_bounds() {
    for max_bytes in [0, 262145] {
        let request = match HistoryRequest::decode(&body_wire(max_bytes)).unwrap() {
            HistoryRequest::Body(request) => request,
            other => panic!("expected body, got {other:?}"),
        };
        assert!(
            request.validate(&limits()).is_err(),
            "maxBytes {max_bytes} must be rejected"
        );
    }
    let request = match HistoryRequest::decode(&body_wire(262144)).unwrap() {
        HistoryRequest::Body(request) => request,
        other => panic!("expected body, got {other:?}"),
    };
    request.validate(&limits()).unwrap();
}

#[test]
fn window_page_round_trip() {
    let page = window_page();
    let value = serde_json::to_value(&page).unwrap();
    assert_eq!(value["kind"], "window");
    assert_eq!(value["historyRevision"], "17");
    assert_eq!(value["committedTail"], "42");
    assert_eq!(value["coverage"]["kind"], "interval");
    assert_eq!(value["items"][1]["durability"]["state"], "sealed");
    assert_eq!(value["items"][1]["durability"]["origin"], "live");
    assert_eq!(value["items"][1]["durability"]["finalItemSeq"], "9");
    assert_eq!(
        value["items"][1]["subItems"][0]["body"]["representation"],
        "display-text"
    );

    let back: HistoryWindowPage = serde_json::from_value(value).unwrap();
    assert_eq!(back, page);
}

#[test]
fn committed_tail_null_is_serialized_and_required() {
    let mut page = window_page();
    page.committed_tail = None;
    page.before_cursor = None;
    page.after_cursor = None;
    let value = serde_json::to_value(&page).unwrap();
    assert!(value["committedTail"].is_null());
    assert!(value["beforeCursor"].is_null());
    assert!(value["afterCursor"].is_null());
    let back: HistoryWindowPage = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(back, page);

    let mut missing = value;
    missing.as_object_mut().unwrap().remove("committedTail");
    assert!(serde_json::from_value::<HistoryWindowPage>(missing).is_err());
}

#[test]
fn durability_origin_rules() {
    let live_missing = HistoryDurability {
        state: HistoryDurabilityState::Sealed,
        origin: HistoryOrigin::Live,
        final_item_seq: None,
    };
    assert!(live_missing.validate().is_err());

    let imported_with_seq = HistoryDurability {
        state: HistoryDurabilityState::Sealed,
        origin: HistoryOrigin::Imported,
        final_item_seq: Some(DecimalU64::new(1)),
    };
    assert!(imported_with_seq.validate().is_err());

    let imported = HistoryDurability {
        state: HistoryDurabilityState::Sealed,
        origin: HistoryOrigin::Imported,
        final_item_seq: None,
    };
    imported.validate().unwrap();

    let live = HistoryDurability {
        state: HistoryDurabilityState::Sealed,
        origin: HistoryOrigin::Live,
        final_item_seq: Some(DecimalU64::new(9)),
    };
    live.validate().unwrap();
}

fn all_sub_items() -> Vec<HistorySubItem> {
    vec![
        text_item("text-0", "hello"),
        HistorySubItem::Reasoning(HistoryTextSubItem {
            sub_item_id: "reasoning-0".to_owned(),
            role: "assistant".to_owned(),
            text: Some("thinking".to_owned()),
            body: None,
        }),
        HistorySubItem::ToolCall(HistoryToolCallSubItem {
            sub_item_id: "tool-call-0".to_owned(),
            invocation_key: "invocation-1".to_owned(),
            title: "Read".to_owned(),
            status: "running".to_owned(),
            input: Some(json!({ "path": "/tmp/a" })),
            body: None,
        }),
        HistorySubItem::ToolResult(HistoryToolResultSubItem {
            sub_item_id: "tool-result-0".to_owned(),
            invocation_key: "invocation-1".to_owned(),
            status: "completed".to_owned(),
            output: Some("ok".to_owned()),
            body: None,
        }),
        HistorySubItem::Reminder(HistoryNoticeSubItem {
            sub_item_id: "reminder-0".to_owned(),
            category: "system".to_owned(),
            title: "Reminder".to_owned(),
            text: Some("remember".to_owned()),
            body: None,
        }),
        HistorySubItem::Summary(HistoryNoticeSubItem {
            sub_item_id: "summary-0".to_owned(),
            category: "compact".to_owned(),
            title: "Summary".to_owned(),
            text: None,
            body: Some(descriptor()),
        }),
        HistorySubItem::Attachment(HistoryAttachmentSubItem {
            sub_item_id: "attachment-0".to_owned(),
            media_type: "image/png".to_owned(),
            label: "diagram".to_owned(),
            body: descriptor(),
        }),
        HistorySubItem::Opaque(HistoryOpaqueSubItem {
            sub_item_id: "opaque-0".to_owned(),
            body: HistoryBodyDescriptor {
                body_ref: "message-43".to_owned(),
                representation: HistoryRepresentation::DisplayJson,
                media_type: "application/json".to_owned(),
                total_bytes: DecimalU64::new(104857600),
                chunk_bytes: 65536,
            },
        }),
    ]
}

#[test]
fn subitem_kinds_round_trip() {
    let items = all_sub_items();
    for item in &items {
        item.validate_bounded(&limits()).unwrap();
    }
    let value = serde_json::to_value(&items).unwrap();
    let kinds: Vec<&str> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "text",
            "reasoning",
            "toolCall",
            "toolResult",
            "reminder",
            "summary",
            "attachment",
            "opaque"
        ]
    );
    let back: Vec<HistorySubItem> = serde_json::from_value(value).unwrap();
    assert_eq!(back, items);
}

#[test]
fn text_subitem_requires_exactly_one_payload() {
    let both = HistorySubItem::Text(HistoryTextSubItem {
        sub_item_id: "text-0".to_owned(),
        role: "assistant".to_owned(),
        text: Some("hello".to_owned()),
        body: Some(descriptor()),
    });
    assert!(both.validate_bounded(&limits()).is_err());

    let neither = HistorySubItem::Text(HistoryTextSubItem {
        sub_item_id: "text-0".to_owned(),
        role: "assistant".to_owned(),
        text: None,
        body: None,
    });
    assert!(neither.validate_bounded(&limits()).is_err());

    text_item("text-0", "hello")
        .validate_bounded(&limits())
        .unwrap();
}

#[test]
fn attachment_requires_body_and_unknown_kind_fails() {
    let missing_body = json!({
        "kind": "attachment",
        "subItemId": "attachment-0",
        "mediaType": "image/png",
        "label": "diagram"
    });
    assert!(serde_json::from_value::<HistorySubItem>(missing_body).is_err());

    let unknown = json!({
        "kind": "video",
        "subItemId": "video-0"
    });
    assert!(serde_json::from_value::<HistorySubItem>(unknown).is_err());
}

#[test]
fn progress_budget_exhaustion_requires_progress() {
    let stalled = HistoryProgress {
        reason: HistoryProgressReason::ScanBudgetExhausted,
        scanned_entries: 10,
        consumed_entries: 0,
    };
    assert!(stalled.validate().is_err());

    let progressed = HistoryProgress {
        reason: HistoryProgressReason::ScanBudgetExhausted,
        scanned_entries: 10,
        consumed_entries: 1,
    };
    progressed.validate().unwrap();

    let complete = HistoryProgress {
        reason: HistoryProgressReason::Complete,
        scanned_entries: 0,
        consumed_entries: 0,
    };
    complete.validate().unwrap();
}

#[test]
fn negotiation_accepts_v2_windowed() {
    let client = HistoryClientCapability {
        versions: vec![2],
        requested_profile: "windowed".to_owned(),
    };
    let server = client.select(&limits()).unwrap();
    assert_eq!(server.version, HISTORY_V2_VERSION);
    assert_eq!(server.selected_profile, HISTORY_PROFILE_WINDOWED);
    assert_eq!(server.method, HISTORY_METHOD);
    assert_eq!(server.notification, HISTORY_CHANGED_NOTIFICATION);
    assert_eq!(server.limits, limits());
}

#[test]
fn negotiation_rejects_unsupported_version_and_profile() {
    let old_version = HistoryClientCapability {
        versions: vec![1],
        requested_profile: "windowed".to_owned(),
    };
    assert_eq!(
        old_version.select(&limits()).unwrap_err(),
        HistoryContractError::UnsupportedVersion
    );

    let other_profile = HistoryClientCapability {
        versions: vec![2],
        requested_profile: "full".to_owned(),
    };
    assert_eq!(
        other_profile.select(&limits()).unwrap_err(),
        HistoryContractError::UnsupportedVersion
    );
}

#[test]
fn limits_defaults_are_valid_and_invalid_limits_fail() {
    let valid = limits();
    valid.validate().unwrap();
    assert_eq!(valid.default_items, 50);
    assert_eq!(valid.max_items, 100);
    assert_eq!(valid.target_page_bytes, 524288);
    assert_eq!(valid.max_response_bytes, 1048576);
    assert_eq!(valid.default_body_chunk_bytes, 65536);
    assert_eq!(valid.max_body_chunk_bytes, 262144);
    assert_eq!(valid.max_windows_per_connection, 8);

    let mut default_above_max = valid.clone();
    default_above_max.default_items = 101;
    assert!(default_above_max.validate().is_err());

    let mut max_too_large = valid.clone();
    max_too_large.max_items = 101;
    assert!(max_too_large.validate().is_err());

    let mut target_too_small = valid.clone();
    target_too_small.target_page_bytes = 65535;
    assert!(target_too_small.validate().is_err());

    let mut target_too_large = valid.clone();
    target_too_large.target_page_bytes = 524289;
    assert!(target_too_large.validate().is_err());

    let mut chunk_too_large = valid.clone();
    chunk_too_large.max_body_chunk_bytes = 262145;
    assert!(chunk_too_large.validate().is_err());

    let mut default_chunk_too_large = valid.clone();
    default_chunk_too_large.default_body_chunk_bytes = 300000;
    assert!(default_chunk_too_large.validate().is_err());

    let mut no_windows = valid;
    no_windows.max_windows_per_connection = 0;
    assert!(no_windows.validate().is_err());
}

#[test]
fn server_capability_meta_round_trip() {
    let client = HistoryClientCapability {
        versions: vec![2],
        requested_profile: "windowed".to_owned(),
    };
    let server = client.select(&limits()).unwrap();
    let meta = server.to_meta().unwrap();
    assert_eq!(meta.len(), 1);
    assert!(meta.contains_key(HISTORY_META_KEY));
    let parsed = HistoryServerCapability::parse_from_meta(&meta)
        .unwrap()
        .unwrap();
    assert_eq!(parsed, server);

    assert_eq!(
        HistoryServerCapability::parse_from_meta(&Map::new()).unwrap(),
        None
    );
}

#[test]
fn client_capability_meta_parsing() {
    let meta = json!({
        "peri.history": {
            "versions": [2],
            "requestedProfile": "windowed"
        }
    })
    .as_object()
    .unwrap()
    .clone();
    let client = HistoryClientCapability::parse_from_meta(&meta)
        .unwrap()
        .unwrap();
    assert_eq!(client.versions, vec![2]);
    assert_eq!(client.requested_profile, "windowed");

    assert_eq!(
        HistoryClientCapability::parse_from_meta(&Map::new()).unwrap(),
        None
    );

    let malformed = json!({
        "peri.history": {
            "versions": "2",
            "requestedProfile": "windowed"
        }
    })
    .as_object()
    .unwrap()
    .clone();
    assert!(HistoryClientCapability::parse_from_meta(&malformed).is_err());
}

#[test]
fn error_data_wire_shape() {
    let data = HistoryContractError::UnsupportedVersion.to_error_data();
    let value = serde_json::to_value(&data).unwrap();
    assert_eq!(value["domain"], "peri.history");
    assert_eq!(value["version"].as_u64(), Some(2));
    assert_eq!(value["reason"], "unsupported_version");
    assert_eq!(value["recovery"], "renegotiate");
    assert_eq!(value["effect"]["execution"], "unchanged");
    assert_eq!(value["effect"]["history"], "unchanged");
    data.validate().unwrap();

    let invalid = HistoryContractError::invalid_request("bad field").to_error_data();
    assert_eq!(invalid.reason, HistoryErrorReason::InvalidRequest);
    assert_eq!(invalid.recovery, HistoryRecovery::None);
}

#[test]
fn effect_ready_requires_runtime_generation() {
    let missing = HistoryEffect {
        execution: ExecutionEffect::Ready,
        history: HistoryEffectState::NotPublished,
        runtime_generation: None,
    };
    assert!(missing.validate().is_err());

    let ready = HistoryEffect {
        execution: ExecutionEffect::Ready,
        history: HistoryEffectState::NotPublished,
        runtime_generation: Some("runtime-9".to_owned()),
    };
    ready.validate().unwrap();

    let data = HistoryErrorData::new(
        HistoryErrorReason::BackendUnavailable,
        HistoryRecovery::Retry,
        ready,
    );
    data.validate().unwrap();
    let value = serde_json::to_value(&data).unwrap();
    assert_eq!(value["effect"]["execution"], "ready");
    assert_eq!(value["effect"]["runtimeGeneration"], "runtime-9");
}

#[test]
fn all_error_reasons_serialize() {
    let reasons = [
        HistoryErrorReason::ProtocolNotNegotiated,
        HistoryErrorReason::UnsupportedVersion,
        HistoryErrorReason::InvalidCursor,
        HistoryErrorReason::CrossWindowCursor,
        HistoryErrorReason::SnapshotInvalidated,
        HistoryErrorReason::WindowExpired,
        HistoryErrorReason::WindowClosed,
        HistoryErrorReason::TicketInvalid,
        HistoryErrorReason::AnchorUnavailable,
        HistoryErrorReason::PermissionDenied,
        HistoryErrorReason::NotFound,
        HistoryErrorReason::IndexNotReady,
        HistoryErrorReason::SourceUnavailable,
        HistoryErrorReason::CapacityExceeded,
        HistoryErrorReason::BudgetExceeded,
        HistoryErrorReason::BackendUnavailable,
        HistoryErrorReason::Timeout,
        HistoryErrorReason::RequestConflict,
        HistoryErrorReason::OperationExpired,
        HistoryErrorReason::OperationInProgress,
        HistoryErrorReason::DeliveryExpired,
        HistoryErrorReason::DeliveryGap,
        HistoryErrorReason::Busy,
        HistoryErrorReason::InvalidRequest,
    ];
    for reason in reasons {
        let value = serde_json::to_value(reason).unwrap();
        assert!(
            value.as_str().is_some(),
            "{reason:?} must serialize as string"
        );
        let back: HistoryErrorReason = serde_json::from_value(value).unwrap();
        assert_eq!(back, reason);
    }
    assert_eq!(
        serde_json::to_value(HistoryErrorReason::ProtocolNotNegotiated).unwrap(),
        "protocol_not_negotiated"
    );
    assert_eq!(
        serde_json::to_value(HistoryErrorReason::OperationInProgress).unwrap(),
        "operation_in_progress"
    );
}

#[test]
fn identity_lengths_are_bounded() {
    let mut request = decode_open(&open_wire("1", latest_anchor(), 50, 0, 524288));
    request.session_id = "x".repeat(512);
    request.validate(&limits()).unwrap();
    request.session_id = "x".repeat(513);
    assert!(matches!(
        request.validate(&limits()),
        Err(HistoryContractError::IdentityTooLong { field: "sessionId" })
    ));

    let mut entry_request = decode_open(&open_wire("1", entry_anchor(), 25, 24, 524288));
    if let HistoryAnchor::Entry(anchor) = &mut entry_request.anchor {
        anchor.entry_id = "x".repeat(513);
    }
    assert!(matches!(
        entry_request.validate(&limits()),
        Err(HistoryContractError::IdentityTooLong { field: "entryId" })
    ));

    let mut page = decode_page(&page_wire(50));
    page.cursor = Some("x".repeat(HISTORY_TOKEN_MAX_BYTES + 1));
    assert!(matches!(
        page.validate(&limits()),
        Err(HistoryContractError::IdentityTooLong { field: "cursor" })
    ));
}

#[test]
fn page_cursor_is_optional_and_bounded_when_present() {
    let mut value = page_wire(50);
    value.as_object_mut().unwrap().remove("cursor");
    let page = decode_page(&value);
    assert!(page.cursor.is_none());
    page.validate(&limits()).unwrap();
}

#[test]
fn page_rejects_unknown_fields_and_wrong_kind() {
    let value = serde_json::to_value(window_page()).unwrap();
    assert!(serde_json::from_value::<HistoryWindowPage>(with_extra(value.clone())).is_err());

    let mut wrong_kind = value;
    wrong_kind["kind"] = json!("initial");
    assert!(serde_json::from_value::<HistoryWindowPage>(wrong_kind).is_err());
}

#[test]
fn nested_enum_variants_reject_unknown_fields() {
    let anchor_extra = json!({
        "kind": "entry",
        "entryId": "e1",
        "source": { "kind": "own" },
        "extra": true
    });
    assert!(serde_json::from_value::<HistoryAnchor>(anchor_extra).is_err());

    let source_extra = json!({
        "kind": "inherited",
        "snapshotId": "snapshot-1",
        "ancestorSessionId": "ancestor-1",
        "extra": true
    });
    assert!(serde_json::from_value::<HistorySource>(source_extra).is_err());

    let sub_item_extra = json!({
        "kind": "text",
        "subItemId": "text-0",
        "role": "assistant",
        "text": "hello",
        "extra": true
    });
    assert!(serde_json::from_value::<HistorySubItem>(sub_item_extra).is_err());

    let coverage_extra = json!({
        "kind": "interval",
        "from": { "kind": "head" },
        "to": { "kind": "tail" },
        "extra": true
    });
    assert!(serde_json::from_value::<HistoryCoverage>(coverage_extra).is_err());
}

#[test]
fn initial_result_round_trip() {
    let result = HistoryInitialResult {
        version: 2,
        kind: HistoryInitialKind::Initial,
        request_key: DecimalU64::new(1),
        execution_ready: true,
        runtime_generation: "runtime-9".to_owned(),
        delivery_id: "delivery-51".to_owned(),
        live_boundary: HistoryLiveBoundary {
            stream_id: "live-7".to_owned(),
            after_seq: DecimalU64::new(120),
        },
        page: {
            let mut page = window_page();
            page.committed_tail = None;
            page.items.clear();
            page.coverage = HistoryCoverage::EmptySnapshot;
            page.progress = HistoryProgress {
                reason: HistoryProgressReason::Complete,
                scanned_entries: 0,
                consumed_entries: 0,
            };
            page.expires_at = None;
            page
        },
    };
    result.validate().unwrap();
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["kind"], "initial");
    assert_eq!(value["liveBoundary"]["afterSeq"], "120");
    assert!(value["page"]["committedTail"].is_null());
    let back: HistoryInitialResult = serde_json::from_value(value).unwrap();
    assert_eq!(back, result);
}
