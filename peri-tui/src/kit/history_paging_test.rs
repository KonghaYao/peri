//! 历史分页纯函数与浏览状态用例（REQ-C01、A11/A20/A23）。
//!
//! 覆盖：响应解码、区间合并（乱序 / 重叠 / 整页更早 / 中间填补）、去重、
//! 插入块槽位包络、触发合并（相同 cursor 只发一次）与身份不匹配时的结算丢弃。
//! 阅读锚点（A20）不在这里测——它要真实视觉索引 `SlotIndex` 与真实发布路径，用例在
//! `kit::message_area::viewport_anchor`（单元）与 `...::viewport_anchor::e2e_tests`（端到端）。

use super::*;
use crate::kit::history_page_consumer::HistoryPageRequest;
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, HistoryWindowConfig, PersistentEntryKey, SegmentDraft,
    SegmentPlacement, WindowIdentity,
};
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};
use serde_json::json;
use serial_test::serial;

fn user_unit(text: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text.to_string()))
}

fn entry(entry_id: &str, position: u64, text: &str) -> PageEntry {
    PageEntry {
        entry_id: entry_id.to_string(),
        position,
        units: vec![user_unit(text)],
    }
}

fn window_texts(window: &im::Vector<TuiRenderUnit>) -> Vec<String> {
    window
        .iter()
        .map(|unit| match unit {
            TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn test_decode_page_response_keeps_sub_item_kind() {
    let raw = json!({
        "sessionId": "s1",
        "snapshotToken": "tok",
        "nextCursor": "c2",
        "items": [
            {
                "entryId": "s1:a",
                "position": 42,
                "oversize": true,
                "subItems": [
                    { "position": 0, "update": { "sessionUpdate": "agent_message_chunk" } },
                    { "position": 1, "systemReminder": { "content": "note" } },
                ],
            },
        ],
    });
    let decoded = decode_page_response(&raw).expect("page");
    assert_eq!(decoded.session_id, "s1");
    assert_eq!(decoded.snapshot_token, "tok");
    assert_eq!(decoded.next_cursor.as_deref(), Some("c2"));
    assert_eq!(decoded.items.len(), 1);
    assert!(decoded.items[0].oversize);
    assert_eq!(decoded.items[0].position, 42);
    assert_eq!(decoded.items[0].sub_items.len(), 2);
    assert!(!decoded.items[0].sub_items[0].system_reminder);
    assert!(decoded.items[0].sub_items[1].system_reminder);
}

#[test]
fn test_decode_page_response_rejects_unknown_shape() {
    assert!(decode_page_response(&json!({ "sessionId": "s1" })).is_none());
    let (token, cursor) = decode_session_cursor(&json!({ "sessionId": "s1" }));
    assert_eq!(token, None);
    assert_eq!(cursor, None);
    let (token, cursor) = decode_session_cursor(&json!({
        "snapshotToken": "tok",
        "nextCursor": null,
    }));
    assert_eq!(token.as_deref(), Some("tok"));
    assert_eq!(cursor, None);
}

#[test]
fn test_merge_page_orders_by_position_and_dedupes() {
    let mut spans = HistorySpans::default();
    let mut window = im::Vector::from(vec![user_unit("own-100")]);
    spans.record(100, "s1:own".to_string(), 0, 1);

    // 乱序页：20 在 10 之前给出，仍必须按 position 落到 100 之前。
    let outcome = merge_page(
        &mut window,
        &mut spans,
        &[entry("s1:b", 20, "b"), entry("s1:a", 10, "a")],
    );
    assert_eq!(outcome.inserted_entries, 2);
    assert_eq!(outcome.inserted_units, 2);
    assert_eq!(outcome.inserted_slot_range, Some((0, 2)));
    assert_eq!(window_texts(&window), vec!["a", "b", "own-100"]);

    // 重试同一页：全部判重，窗口不变。
    let outcome = merge_page(
        &mut window,
        &mut spans,
        &[entry("s1:b", 20, "b"), entry("s1:a", 10, "a")],
    );
    assert_eq!(outcome.inserted_entries, 0);
    assert_eq!(outcome.duplicates, 2);
    assert_eq!(outcome.inserted_slot_range, None);
    assert_eq!(window_texts(&window), vec!["a", "b", "own-100"]);
}

#[test]
fn test_merge_page_inserts_into_middle_interval() {
    let mut spans = HistorySpans::default();
    let mut window = im::Vector::from(vec![user_unit("a"), user_unit("c")]);
    spans.record(10, "s1:a".to_string(), 0, 1);
    spans.record(30, "s1:c".to_string(), 1, 1);

    let outcome = merge_page(&mut window, &mut spans, &[entry("s1:b", 20, "b")]);
    assert_eq!(outcome.inserted_entries, 1);
    assert_eq!(outcome.inserted_slot_range, Some((1, 2)));
    assert_eq!(window_texts(&window), vec!["a", "b", "c"]);
    // 后移后的区间槽位仍然正确：再插一条更早的条目落在最前。
    merge_page(&mut window, &mut spans, &[entry("s1:z", 5, "z")]);
    assert_eq!(window_texts(&window), vec!["z", "a", "b", "c"]);
}

/// 请求通道全进程只装一次：`HISTORY_PAGES_TX` 是 `OnceLock`，接受端必须活到测试
/// 进程结束——否则后续用例里 `send` 失败，触发侧会把认领回滚成 no-op。
/// `pub(crate)`：滚动事件路径的触发用例（`message_area::scroll_test`）复用同一份
/// 接收端，避免出现第二个空接收端（断言真空）。
pub(crate) fn pages_channel()
-> &'static parking_lot::Mutex<tokio::sync::mpsc::UnboundedReceiver<HistoryPageRequest>> {
    static RECEIVER: std::sync::OnceLock<
        parking_lot::Mutex<tokio::sync::mpsc::UnboundedReceiver<HistoryPageRequest>>,
    > = std::sync::OnceLock::new();
    RECEIVER.get_or_init(|| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = atoms::HISTORY_PAGES_TX.set(tx);
        parking_lot::Mutex::new(rx)
    })
}

/// 清掉别的用例留下的请求，保证本用例只观察自己派发的那一条。
pub(crate) fn drain_pages_channel() {
    let rx = pages_channel();
    while rx.lock().try_recv().is_ok() {}
}

#[test]
#[serial]
fn test_request_older_page_if_due_merges_concurrent_triggers() {
    crate::kit::atoms::init_atoms();
    reset_browsing_state();
    drain_pages_channel();

    // 未建立浏览身份：不请求。
    assert!(!request_older_page_if_due(true));
    note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));
    // 远离向前边界：不请求。
    assert!(!request_older_page_if_due(false));
    // 贴边：派发一次，并把请求认领为在途。
    assert!(request_older_page_if_due(true));
    // 相同 cursor + limit 的并发触发被合并（不重复派发）。
    assert!(!request_older_page_if_due(true));
    let rx = pages_channel();
    let request = rx.lock().try_recv().expect("one dispatched request");
    assert_eq!(request.session_id, "s1");
    assert_eq!(request.snapshot_token, "tok");
    assert_eq!(request.cursor, "c1");
    assert_eq!(request.limit, PAGE_LIMIT);
    assert!(rx.lock().try_recv().is_err(), "并发触发只产生一个请求");
}

#[test]
#[serial]
fn test_settle_request_drops_response_for_stale_identity() {
    crate::kit::atoms::init_atoms();
    reset_browsing_state();
    note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));
    drain_pages_channel();
    assert!(request_older_page_if_due(true));
    let in_flight = atoms::HISTORY_PAGING
        .state()
        .read()
        .in_flight
        .clone()
        .expect("claimed");

    // 切会话后旧响应必须被丢弃。
    reset_browsing_state();
    assert_eq!(
        settle_request(&in_flight, Ok(Some("c2".to_string()))),
        None,
        "身份不匹配的响应不得推进游标"
    );

    // 同一次浏览内：成功推进游标。
    note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));
    drain_pages_channel();
    assert!(request_older_page_if_due(true));
    let in_flight = atoms::HISTORY_PAGING
        .state()
        .read()
        .in_flight
        .clone()
        .expect("claimed");
    assert_eq!(
        settle_request(&in_flight, Ok(Some("c2".to_string()))),
        Some(Some("c2".to_string()))
    );
    assert_eq!(
        atoms::HISTORY_PAGING.state().read().next_cursor.as_deref(),
        Some("c2")
    );

    // 失败：保留原游标并记录原因（可重试），不推进。
    // 退避窗口由 `last_request_at` 控制，这里显式清掉——不靠 sleep 让用例变脆。
    atoms::HISTORY_PAGING.state().write().last_request_at = None;
    assert!(request_older_page_if_due(true), "失败后允许重试同一页");
    let in_flight = atoms::HISTORY_PAGING
        .state()
        .read()
        .in_flight
        .clone()
        .expect("retry claimed");
    assert_eq!(settle_request(&in_flight, Err("boom".to_string())), None);
    let state = atoms::HISTORY_PAGING.state().read().clone();
    assert_eq!(state.next_cursor.as_deref(), Some("c2"));
    assert_eq!(state.last_error.as_deref(), Some("boom"));
    assert!(state.in_flight.is_none());
}

#[test]
#[serial]
fn test_invalidate_browsing_stops_paging_for_session() {
    crate::kit::atoms::init_atoms();
    reset_browsing_state();
    note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));
    drain_pages_channel();

    // 别的会话的失效通知不影响当前浏览。
    invalidate_browsing("s2", "range expired");
    assert!(atoms::HISTORY_PAGING.state().read().next_cursor.is_some());

    invalidate_browsing("s1", "range expired");
    let state = atoms::HISTORY_PAGING.state().read().clone();
    assert!(state.next_cursor.is_none());
    assert!(state.snapshot_token.is_none());
    assert_eq!(state.last_error.as_deref(), Some("range expired"));
    // 失效后不再派发（需要下一次初始 load 重建浏览身份）。
    assert!(!request_older_page_if_due(true));
}

// ---------------------------------------------------------------------------
// S4 窗口 wire 与 ResidentIndex 派生适配
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_window_mode_does_not_claim_legacy_page_channel() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(true);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(true);
    crate::kit::history_page_consumer::reset_window_paging_state();
    reset_browsing_state();
    note_initial_cursor("s1", Some("token".into()), Some("legacy-cursor".into()));
    assert!(!request_older_page_if_due(true));
    let legacy = atoms::HISTORY_PAGING.state().read().clone();
    assert!(
        legacy.in_flight.is_none(),
        "window mode must not claim legacy page"
    );
    reset_browsing_state();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
}

#[test]
fn test_decode_window_page_response_matches_s4_wire() {
    let raw = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-7",
        "structureVersion": 12,
        "snapshotToken": "snap-9",
        "committedTail": "opaque-tail",
        "checkpointId": "checkpoint-1",
        "readTicket": "ticket-1",
        "beforeCursor": "before-1",
        "afterCursor": null,
        "items": [
            {
                "entryId": "s1:m1",
                "source": "own",
                "position": 42,
                "oversize": false,
                "subItems": [
                    { "position": 0, "update": { "sessionUpdate": "agent_message_chunk" } },
                    { "position": 1, "systemReminder": { "content": "note" } }
                ]
            },
            {
                "entryId": "s1:m2",
                "source": "own",
                "position": 43,
                "oversize": true,
                "body": {
                    "totalBytes": 2_000_000,
                    "chunkSize": 32_768,
                    "readTicket": "inline-ticket-2",
                    "snapshotToken": "snap-inline"
                },
                "subItems": []
            },
            {
                "entryId": "s1:m3",
                "source": "own",
                "position": 44,
                "oversize": false,
                "subItems": []
            },
            {
                "entryId": "s1:m4",
                "source": "own",
                "position": 45,
                "oversize": true,
                "body": {
                    "totalBytes": 999,
                    "chunkSize": 1,
                    "readTicket": "inline-ticket-4"
                },
                "subItems": []
            }
        ],
        "bodyDescriptors": [
            {
                "entryId": "s1:m3",
                "totalBytes": 4_000_000,
                "chunkSize": 65_536,
                "readTicket": "top-ticket-3"
            },
            {
                "entryId": "s1:m4",
                "totalBytes": 123,
                "chunkSize": 1,
                "readTicket": "top-ticket-4",
                "snapshotToken": "snap-top-4"
            },
            {
                "entryId": "s1:m5",
                "source": { "inherited": { "frozenSnapshotId": "snap-ancestor" } },
                "totalBytes": 5_000_000,
                "chunkSize": 65_536,
                "readTicket": "ticket-1"
            }
        ],
        "rewindInvalidation": { "status": "active", "reopenRequired": false }
    });
    let decoded = decode_window_page_response(&raw).expect("window response");
    assert_eq!(decoded.session_id, "s1");
    assert_eq!(decoded.identity.session_generation, "gen-7");
    assert_eq!(decoded.identity.structure_version, 12);
    assert_eq!(decoded.identity.snapshot_token, "snap-9");
    assert_eq!(decoded.committed_tail.as_deref(), Some("opaque-tail"));
    assert_eq!(decoded.checkpoint_id.as_deref(), Some("checkpoint-1"));
    assert_eq!(decoded.read_ticket, "ticket-1");
    assert_eq!(decoded.before_cursor.as_deref(), Some("before-1"));
    assert_eq!(decoded.after_cursor, None);
    assert!(!decoded.reopen_required);
    assert_eq!(decoded.body_descriptors.len(), 3);
    assert_eq!(decoded.descriptor_only_items, 1);
    assert_eq!(decoded.items.len(), 5);

    assert_eq!(decoded.items[0].source, HistorySource::Own);
    assert_eq!(decoded.items[0].position, Some(42));
    assert_eq!(decoded.items[0].sub_items.len(), 2);
    assert!(!decoded.items[0].sub_items[0].system_reminder);
    assert!(decoded.items[0].sub_items[1].system_reminder);
    assert!(decoded.items[0].descriptor.is_none());

    let inline = &decoded.items[1];
    assert_eq!(inline.entry_id, "s1:m2");
    assert!(!inline.descriptor_only);
    let descriptor = inline.descriptor.as_ref().expect("inline descriptor");
    assert_eq!(descriptor.total_bytes, 2_000_000);
    assert_eq!(descriptor.chunk_size, 32_768);
    assert_eq!(descriptor.read_ticket, "inline-ticket-2");
    assert_eq!(descriptor.snapshot_token.as_deref(), Some("snap-inline"));

    let top_level = &decoded.items[2];
    assert_eq!(top_level.entry_id, "s1:m3");
    assert!(top_level.oversize, "top-level descriptor marks oversize");
    let descriptor = top_level.descriptor.as_ref().expect("top descriptor");
    assert_eq!(descriptor.total_bytes, 4_000_000);
    assert_eq!(descriptor.read_ticket, "top-ticket-3");
    assert_eq!(descriptor.snapshot_token.as_deref(), Some("snap-9"));

    let conflict = &decoded.items[3];
    assert_eq!(conflict.entry_id, "s1:m4");
    let descriptor = conflict.descriptor.as_ref().expect("inline wins");
    assert_eq!(
        descriptor.total_bytes, 999,
        "inline body wins over top-level"
    );
    assert_eq!(descriptor.read_ticket, "inline-ticket-4");
    assert_eq!(descriptor.snapshot_token.as_deref(), Some("snap-9"));

    let orphan = &decoded.items[4];
    assert_eq!(orphan.entry_id, "s1:m5");
    assert!(orphan.descriptor_only);
    assert!(orphan.oversize);
    assert_eq!(orphan.position, None);
    assert_eq!(orphan.source, HistorySource::inherited("snap-ancestor"));
    let orphan_descriptor = orphan.descriptor.as_ref().expect("orphan descriptor");
    assert_eq!(orphan_descriptor.total_bytes, 5_000_000);
    assert_eq!(orphan_descriptor.snapshot_token.as_deref(), Some("snap-9"));

    let no_descriptors = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-7",
        "structureVersion": 12,
        "snapshotToken": "snap-9",
        "readTicket": "ticket-1",
        "items": []
    });
    let decoded = decode_window_page_response(&no_descriptors).expect("empty response");
    assert!(decoded.body_descriptors.is_empty());
    assert_eq!(decoded.descriptor_only_items, 0);
    assert!(decoded.items.is_empty());

    let invalid = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-7",
        "structureVersion": 12,
        "snapshotToken": "snap-9",
        "readTicket": "ticket-1",
        "items": [
            { "entryId": "s1:missing-position", "source": "own", "subItems": [] }
        ]
    });
    assert!(
        decode_window_page_response(&invalid).is_none(),
        "own entries without position are malformed and drop the whole page"
    );
}

#[test]
fn test_decode_inherited_snapshot_and_source() {
    let raw = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-1",
        "structureVersion": 1,
        "snapshotToken": "snap-1",
        "readTicket": "ticket-1",
        "inheritedUnsupported": false,
        "inherited": {
            "snapshotId": "snap-frozen",
            "ancestorThreadId": "ancestor-session",
            "firstEntryId": "s1:a",
            "lastEntryId": "s1:b",
            "entryCount": 2,
            "supported": true
        },
        "items": [{
            "entryId": "s1:a",
            "source": {"inherited": {
                "frozenSnapshotId": "snap-frozen",
                "ancestorThreadId": "ancestor-session"
            }},
            "position": null,
            "subItems": [{
                "position": 0,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "frozen"},
                    "messageId": "a"
                }
            }]
        }]
    });
    let decoded = decode_window_page_response(&raw).expect("inherited response");
    let inherited = decoded.inherited.as_ref().expect("inherited meta");
    assert_eq!(inherited.snapshot_id, "snap-frozen");
    assert_eq!(
        inherited.ancestor_thread_id.as_deref(),
        Some("ancestor-session")
    );
    assert_eq!(inherited.entry_count, 2);
    assert!(inherited.supported);
    assert!(!decoded.inherited_unsupported);
    assert_eq!(decoded.items[0].position, None);
    assert_eq!(
        decoded.items[0].source,
        HistorySource::inherited_with_ancestor("snap-frozen", Some("ancestor-session".into()))
    );
    assert_eq!(
        decoded.items[0].source.ancestor_thread_id(),
        Some("ancestor-session")
    );
}

#[test]
fn test_window_body_descriptors_use_snake_case_and_reject_duplicates() {
    let base = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-7",
        "structureVersion": 12,
        "snapshotToken": "snap-9",
        "readTicket": "ticket-1",
        "items": []
    });
    let mut snake = base.clone();
    snake["body_descriptors"] = json!([
        {
            "entryId": "s1:orphan",
            "totalBytes": 10,
            "chunkSize": 1,
            "readTicket": "ticket-1"
        }
    ]);
    let decoded = decode_window_page_response(&snake).expect("snake_case descriptors");
    assert_eq!(decoded.descriptor_only_items, 1);
    assert_eq!(decoded.items[0].entry_id, "s1:orphan");

    let mut duplicate = base;
    duplicate["bodyDescriptors"] = json!([
        {
            "entryId": "s1:orphan",
            "totalBytes": 10,
            "chunkSize": 1,
            "readTicket": "ticket-1"
        },
        {
            "entryId": "s1:orphan",
            "totalBytes": 20,
            "chunkSize": 1,
            "readTicket": "ticket-1"
        }
    ]);
    assert!(
        decode_window_page_response(&duplicate).is_none(),
        "duplicate descriptors are ambiguous, not silently deduplicated"
    );
}

#[test]
fn test_window_request_encodes_s4_open_page_and_seek_shapes() {
    let open = WindowRequest::Open(WindowOpenRequest {
        anchor: WindowRequestAnchor::Latest,
        direction: WindowDirection::Around,
        limit: WINDOW_PAGE_LIMIT,
        before_limit: Some(10),
        after_limit: Some(20),
        byte_budget: WINDOW_DEFAULT_BYTE_BUDGET,
    })
    .to_value();
    assert_eq!(open["kind"], "open");
    assert_eq!(open["anchor"]["kind"], "latest");
    assert_eq!(open["direction"], "around");
    assert_eq!(open["limit"], WINDOW_PAGE_LIMIT);
    assert_eq!(open["beforeLimit"], 10);
    assert_eq!(open["afterLimit"], 20);
    assert_eq!(open["byteBudget"], WINDOW_DEFAULT_BYTE_BUDGET);
    for legacy in ["cursor", "loadGeneration", "cwd", "mcpServers"] {
        assert!(
            open.get(legacy).is_none(),
            "history is mutually exclusive with legacy top-level {legacy}"
        );
    }

    let page = WindowRequest::Page(WindowPageRequest {
        snapshot_token: "snap-9".into(),
        read_ticket: "ticket-1".into(),
        cursor: WindowPageCursor::Before("before-1".into()),
        limit: 20,
    })
    .to_value();
    assert_eq!(page["kind"], "page");
    assert_eq!(page["beforeCursor"], "before-1");
    assert!(page.get("afterCursor").is_none());
    assert_eq!(page["readTicket"], "ticket-1");

    let seek = WindowRequest::Seek(WindowSeekRequest {
        snapshot_token: "snap-9".into(),
        read_ticket: "ticket-1".into(),
        anchor: PersistentEntryKey::own("s1:m1"),
        before_limit: 10,
        after_limit: 30,
    })
    .to_value();
    assert_eq!(seek["kind"], "seek");
    assert_eq!(seek["anchor"]["entryId"], "s1:m1");
    assert_eq!(seek["anchor"]["source"], "own");
    assert_eq!(seek["beforeLimit"], 10);
    assert_eq!(seek["afterLimit"], 30);
}

#[test]
fn test_window_source_wire_matches_s4_serde() {
    use peri_acp_types::history_window::HistorySource as WireHistorySource;

    let own_wire = serde_json::to_value(WireHistorySource::Own).expect("own wire");
    let inherited_wire =
        serde_json::to_value(WireHistorySource::inherited("snap-1")).expect("inherited wire");
    assert_eq!(own_wire, json!("own"));
    assert_eq!(
        inherited_wire,
        json!({ "inherited": { "frozenSnapshotId": "snap-1" } })
    );

    let raw = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-7",
        "structureVersion": 12,
        "snapshotToken": "snap-9",
        "readTicket": "ticket-1",
        "items": [
            { "entryId": "s1:own", "source": own_wire, "position": 1, "subItems": [] },
            { "entryId": "s1:inherited", "source": inherited_wire, "subItems": [] }
        ]
    });
    let decoded = decode_window_page_response(&raw).expect("response");
    assert_eq!(decoded.items[0].source, HistorySource::Own);
    assert_eq!(decoded.items[1].source, HistorySource::inherited("snap-1"));

    let seek = WindowRequest::Seek(WindowSeekRequest {
        snapshot_token: "snap-9".into(),
        read_ticket: "ticket-1".into(),
        anchor: PersistentEntryKey::new(HistorySource::inherited("snap-1"), "s1:inherited"),
        before_limit: 10,
        after_limit: 30,
    })
    .to_value();
    assert_eq!(
        seek["anchor"]["source"],
        serde_json::to_value(WireHistorySource::inherited("snap-1")).expect("wire")
    );
}

#[test]
fn test_window_request_validation_rejects_bad_shapes() {
    let base = WindowRequest::Open(WindowOpenRequest {
        anchor: WindowRequestAnchor::Latest,
        direction: WindowDirection::Around,
        limit: 50,
        before_limit: None,
        after_limit: None,
        byte_budget: 1024,
    });
    assert!(base.validate().is_ok());
    let mut invalid = base.clone();
    if let WindowRequest::Open(request) = &mut invalid {
        request.limit = 0;
    }
    assert!(invalid.validate().is_err());
    let mut invalid = base;
    if let WindowRequest::Open(request) = &mut invalid {
        request.byte_budget = 0;
    }
    assert!(invalid.validate().is_err());

    let page = WindowRequest::Page(WindowPageRequest {
        snapshot_token: String::new(),
        read_ticket: "ticket".into(),
        cursor: WindowPageCursor::Before("cursor".into()),
        limit: 20,
    });
    assert!(page.validate().is_err());
    let seek = WindowRequest::Seek(WindowSeekRequest {
        snapshot_token: "snap".into(),
        read_ticket: "ticket".into(),
        anchor: PersistentEntryKey::own(""),
        before_limit: 10,
        after_limit: 10,
    });
    assert!(seek.validate().is_err());
}

#[test]
fn test_viewport_at_backward_boundary_uses_bottom_margin_and_content_fit() {
    assert!(!viewport_at_backward_boundary(false, 0, 100, 10));
    assert!(viewport_at_backward_boundary(false, 88, 100, 10));
    assert!(viewport_at_backward_boundary(false, 90, 100, 10));
    assert!(viewport_at_backward_boundary(true, 0, 100, 10));
    assert!(viewport_at_backward_boundary(false, 0, 5, 10));
    assert!(!viewport_at_backward_boundary(false, 0, 0, 10));
    assert!(!viewport_at_backward_boundary(false, 0, 100, 0));
}

#[test]
fn test_window_generation_rejects_stale_snapshot_or_generation() {
    let raw = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-7",
        "structureVersion": 12,
        "snapshotToken": "snap-9",
        "readTicket": "ticket-1",
        "beforeCursor": null,
        "afterCursor": null,
        "items": [],
    });
    let response = decode_window_page_response(&raw).expect("response");
    let current = WindowGeneration {
        session_id: "s1".into(),
        session_generation: "gen-7".into(),
        structure_version: 12,
        snapshot_token: "snap-9".into(),
        window_generation: 3,
        request_id: "req-1".into(),
    };
    assert!(current.accepts(&response, 3));
    assert!(
        !current.accepts(&response, 4),
        "local window generation is part of the identity"
    );

    let mut stale = current.clone();
    stale.snapshot_token = "snap-other".into();
    assert!(!stale.accepts(&response, 3));
    let mut stale = current;
    stale.structure_version = 13;
    assert!(!stale.accepts(&response, 3));
}

#[test]
fn test_history_spans_derive_slots_from_resident_index() {
    let mut window = HistoryWindow::new(HistoryWindowConfig::enabled());
    window.begin_window(WindowIdentity::new("s1", "gen-7", 12, "snap-9"));
    let entry_a = HistoryEntryView::new(
        PersistentEntryKey::own("s1:a"),
        Some(10),
        2,
        100,
        120,
        Vec::new(),
    );
    let entry_b = HistoryEntryView::new(
        PersistentEntryKey::own("s1:b"),
        Some(20),
        1,
        50,
        60,
        Vec::new(),
    );
    window
        .register_segment(
            SegmentDraft::new(HistorySource::Own, vec![entry_a], 100, 120),
            SegmentPlacement::Tail,
        )
        .expect("segment a");
    window
        .register_segment(
            SegmentDraft::new(HistorySource::Own, vec![entry_b], 50, 60),
            SegmentPlacement::Tail,
        )
        .expect("segment b");
    let inherited = HistoryEntryView::new(
        PersistentEntryKey::new(HistorySource::inherited("snap-ancestor"), "ancestor:1"),
        None,
        1,
        30,
        40,
        Vec::new(),
    );
    window
        .register_segment(
            SegmentDraft::new(
                HistorySource::inherited("snap-ancestor"),
                vec![inherited],
                30,
                40,
            ),
            SegmentPlacement::Tail,
        )
        .expect("inherited segment");

    let spans = HistorySpans::from_resident_window(&window);
    assert!(spans.contains("s1:a"));
    assert!(spans.contains("s1:b"));
    assert!(
        !spans.contains("ancestor:1"),
        "inherited entries have no position and stay outside legacy spans"
    );
    assert_eq!(spans.slots(), vec![(0, 2), (2, 1)]);
}
