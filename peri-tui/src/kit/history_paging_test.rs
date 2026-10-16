//! 历史分页纯函数与浏览状态用例（REQ-C01、A11/A20/A23）。
//!
//! 覆盖：响应解码、区间合并（乱序 / 重叠 / 整页更早 / 中间填补）、去重、
//! 插入块槽位包络、触发合并（相同 cursor 只发一次）与身份不匹配时的结算丢弃。
//! 阅读锚点（A20）不在这里测——它要真实视觉索引 `SlotIndex` 与真实发布路径，用例在
//! `kit::message_area::viewport_anchor`（单元）与 `...::viewport_anchor::e2e_tests`（端到端）。

use super::*;
use crate::kit::history_page_consumer::HistoryPageRequest;
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
