//! Paging triggers regression scenarios.

use super::event;
use serial_test::serial;

/// 滚动事件路径的决策层：视口贴向已载入窗口顶端 → 必须派发取页，
/// 且请求带冻结的会话身份与游标（走真实 `HISTORY_PAGES_TX` 通道观察）。
#[test]
#[serial]
fn test_scroll_lands_at_top_dispatches_older_page_request() {
    crate::kit::atoms::init_atoms();
    crate::kit::history_paging::reset_browsing_state();
    crate::kit::history_paging::tests::drain_pages_channel();
    crate::kit::history_paging::note_initial_cursor(
        "s1",
        Some("tok".to_string()),
        Some("c1".to_string()),
    );
    let rx = crate::kit::history_paging::tests::pages_channel();

    // 离顶端 5 行：未贴边，不派发。
    assert!(!event::request_older_page_at_scroll_facts(
        false, 5, 100, 20
    ));
    assert!(rx.lock().try_recv().is_err(), "未贴边不派发任何请求");

    // 滚轮落地后 offset=0：贴向前边界，必须派发一次。
    assert!(
        event::request_older_page_at_scroll_facts(false, 0, 100, 20),
        "贴向前边界 → 派发取页"
    );
    let request = rx.lock().try_recv().expect("one dispatched request");
    assert_eq!(request.session_id, "s1");
    assert_eq!(request.snapshot_token, "tok");
    assert_eq!(request.cursor, "c1");
    assert_eq!(request.limit, crate::kit::history_paging::PAGE_LIMIT);
    // 相同 cursor 已在途 → 后续滚动事件合并为 no-op（REQ-C01-2）。
    assert!(!event::request_older_page_at_scroll_facts(
        false, 0, 100, 20
    ));
    assert!(rx.lock().try_recv().is_err(), "在途请求只派发一次");
}

/// 离开向前边界时不触发（避免滚动事件路径无谓发请求）。
/// 向下滚动到最底部：窗口模式派发 after 页；到达末尾/在途时停止。
#[test]
#[serial]
fn test_scroll_at_bottom_dispatches_window_after_page_and_stops() {
    use crate::kit::history_page_consumer::WindowConsumerRequest;
    use crate::kit::history_paging::WindowPageCursor;
    use crate::kit::history_window::WindowIdentity;

    crate::kit::atoms::init_atoms();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(true);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(true);
    crate::kit::history_page_consumer::reset_window_paging_state();
    crate::kit::atoms::HISTORY_WINDOW_GENERATION.set(1);
    {
        let handle = crate::kit::atoms::HISTORY_WINDOW_PAGING.state();
        let mut state = handle.write();
        state.session_id = "s1".into();
        state.identity = Some(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
        state.window_generation = 1;
        state.read_ticket = Some("ticket-1".into());
        state.after_cursor = Some("after-1".into());
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(crate::kit::atoms::HISTORY_WINDOW_TX.set(tx).is_ok());

    assert!(event::request_newer_page_at_scroll_facts(
        false, 80, 100, 20
    ));
    let request = rx.try_recv().expect("after page request");
    let request_id = match &request {
        WindowConsumerRequest::Page {
            context, cursor, ..
        } => {
            assert_eq!(cursor, &WindowPageCursor::After("after-1".into()));
            context.request_id.clone()
        }
        other => panic!("expected after page, got {other:?}"),
    };
    assert!(
        !event::request_newer_page_at_scroll_facts(false, 80, 100, 20),
        "same in-flight window page must merge"
    );
    assert!(rx.try_recv().is_err());

    crate::kit::history_page_consumer::fail_window_request(&request_id, "test done", false);
    crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .write()
        .after_cursor = None;
    assert!(!event::request_newer_page_at_scroll_facts(
        false, 80, 100, 20
    ));

    // 视口无锚点 + 桥接发布 gap 目标：滚动触发 seek，内容返回前不跳底。
    crate::kit::atoms::WINDOW_VIEWPORT.set(None);
    crate::kit::atoms::WINDOW_GAP_REQUEST.set(vec![
        crate::kit::history_paging::WindowGapRequest::Seek {
            gap_id: crate::kit::history_window::GapId(1),
            anchor: crate::kit::history_window::PersistentEntryKey::own("s1:missing"),
        },
    ]);
    assert!(event::request_gap_page_after_scroll(None));
    let gap_request = rx.try_recv().expect("gap request");
    assert!(matches!(
        &gap_request,
        WindowConsumerRequest::Seek { anchor, .. }
            if anchor == &crate::kit::history_window::PersistentEntryKey::own("s1:missing")
    ));
    assert!(
        crate::kit::atoms::WINDOW_GAP_REQUEST
            .state()
            .read()
            .is_empty()
    );

    // 双向缺口候选：向下滚只消费 Page(After)，向上滚消费 Page(Before)。
    crate::kit::history_page_consumer::fail_window_request(
        match &gap_request {
            WindowConsumerRequest::Seek { context, .. } => &context.request_id,
            other => panic!("expected seek, got {other:?}"),
        },
        "test done",
        false,
    );
    crate::kit::atoms::WINDOW_GAP_REQUEST.set(vec![
        crate::kit::history_paging::WindowGapRequest::Page {
            gap_id: crate::kit::history_window::GapId(2),
            cursor: WindowPageCursor::Before("before-gap".into()),
        },
        crate::kit::history_paging::WindowGapRequest::Page {
            gap_id: crate::kit::history_window::GapId(2),
            cursor: WindowPageCursor::After("after-gap".into()),
        },
    ]);
    assert!(event::request_gap_page_after_scroll(Some(
        crate::kit::history_window::GapSeekDirection::After
    )));
    let after_request = rx.try_recv().expect("after gap request");
    let after_request_id = match &after_request {
        WindowConsumerRequest::Page {
            context, cursor, ..
        } => {
            assert_eq!(cursor, &WindowPageCursor::After("after-gap".into()));
            context.request_id.clone()
        }
        other => panic!("expected after gap page, got {other:?}"),
    };
    assert!(
        crate::kit::atoms::WINDOW_GAP_REQUEST
            .state()
            .read()
            .iter()
            .all(|request| request
                .matches_direction(Some(crate::kit::history_window::GapSeekDirection::Before))),
        "consuming one direction must keep the other candidate"
    );

    crate::kit::history_page_consumer::fail_window_request(&after_request_id, "test done", false);
    assert!(event::request_gap_page_after_scroll(Some(
        crate::kit::history_window::GapSeekDirection::Before
    )));
    let before_request = rx.try_recv().expect("before gap request");
    assert!(matches!(
        before_request,
        WindowConsumerRequest::Page {
            cursor: WindowPageCursor::Before(ref cursor),
            ..
        } if cursor == "before-gap"
    ));
    assert!(
        crate::kit::atoms::WINDOW_GAP_REQUEST
            .state()
            .read()
            .is_empty()
    );

    crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    crate::kit::history_page_consumer::reset_window_paging_state();
}

#[test]
#[serial]
fn test_scroll_away_from_top_does_not_dispatch() {
    crate::kit::atoms::init_atoms();
    crate::kit::history_paging::reset_browsing_state();
    crate::kit::history_paging::tests::drain_pages_channel();
    crate::kit::history_paging::note_initial_cursor(
        "s1",
        Some("tok".to_string()),
        Some("c1".to_string()),
    );

    // 浏览态、视口离顶端很远 → 不派发。
    assert!(!event::request_older_page_at_scroll_facts(
        false, 40, 200, 20
    ));
    // 跟随态且内容超一屏（看的是尾部）→ 不派发。
    assert!(!event::request_older_page_at_scroll_facts(
        true, 180, 200, 20
    ));
    assert!(
        crate::kit::history_paging::tests::pages_channel()
            .lock()
            .try_recv()
            .is_err(),
        "不派发任何请求"
    );
    assert!(
        crate::kit::atoms::HISTORY_PAGING
            .state()
            .read()
            .in_flight
            .is_none()
    );
}

/// V2：同一滚动事件刚排队的 older-page 不得被 gap seek 取消；方向未知不 seek。
#[test]
#[serial]
fn test_v2_scroll_does_not_cancel_in_flight_older_page() {
    use peri_acp_types::history_v2::{
        DecimalU64, HISTORY_V2_VERSION, HistoryBoundary, HistoryBoundaryPosition,
        HistoryBoundarySide, HistoryDirection, HistoryLimits,
    };
    use serde_json::json;

    use crate::kit::history_v2::paging::{HistoryV2Gap, HistoryV2InFlightKind};
    use crate::kit::history_window::GapSeekDirection;

    struct V2Guard;

    impl Drop for V2Guard {
        fn drop(&mut self) {
            crate::kit::atoms::HISTORY_V2_CAPABLE.set(false);
            crate::kit::atoms::HISTORY_V2_STATE
                .set(crate::kit::history_v2::HistoryV2State::default());
            *crate::kit::atoms::HISTORY_V2_TX
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }

    crate::kit::atoms::init_atoms();
    crate::kit::history_v2::consumer::reset_request_key_for_test();
    let _guard = V2Guard;
    crate::kit::atoms::HISTORY_V2_CAPABLE.set(true);

    let limits = HistoryLimits::default();
    let initial = json!({
        "_meta": {"peri.history": {
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
                "historyRevision": "17",
                "committedTail": "19",
                "projectionVersion": 1,
                "readTicket": "ticket-1",
                "beforeCursor": "before-1",
                "afterCursor": "after-1",
                "items": [{
                    "entryId": "s1:e5",
                    "source": {"kind": "own"},
                    "contentVersion": "1",
                    "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "3"},
                    "order": {"region": "own", "ordinal": "5"},
                    "subItems": [{
                        "kind": "text",
                        "subItemId": "s1:e5:0",
                        "role": "assistant",
                        "text": "anchor",
                    }],
                }],
                "coverage": {
                    "kind": "interval",
                    "from": {"kind": "head"},
                    "to": {"kind": "tail"},
                },
                "progress": {"reason": "complete", "scannedEntries": 1, "consumedEntries": 1},
            },
        }},
    });
    let mut state = crate::kit::history_v2::initial::admit_load_response(
        &initial,
        "s1",
        DecimalU64::new(1),
        &limits,
    )
    .expect("initial admitted");
    state.window_mut().expect("window").gaps.push(HistoryV2Gap {
        from: HistoryBoundary::Head,
        to: HistoryBoundary::Position(HistoryBoundaryPosition {
            region: "own".into(),
            ordinal: DecimalU64::new(5),
            side: HistoryBoundarySide::Before,
        }),
    });
    let page = state
        .window_mut()
        .expect("window")
        .request_page(
            DecimalU64::new(2),
            HistoryDirection::Before,
            limits.default_items,
            &limits,
        )
        .expect("older page request");
    let request_key = page.context.request_key;
    let epoch = state.window().expect("window").paging.view_epoch;
    crate::kit::atoms::HISTORY_V2_STATE.set(state);
    crate::kit::history_v2::consumer::seed_request_key(DecimalU64::new(2));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    *crate::kit::atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);

    // 向上滚动：本事件已排队的 older-page 不得被 gap seek 取消。
    assert!(!event::request_gap_page_after_scroll(Some(
        GapSeekDirection::Before
    )));
    assert!(rx.try_recv().is_err(), "在途 page 存在时不得发 seek");
    {
        let published = crate::kit::atoms::HISTORY_V2_STATE.state();
        let published = published.read();
        let window = published.window().expect("window");
        assert_eq!(window.paging.view_epoch, epoch, "不得推进导航代次");
        let flight = window
            .paging
            .in_flight
            .as_ref()
            .expect("older-page 请求必须保留");
        assert_eq!(flight.context.request_key, request_key);
        assert!(matches!(
            flight.kind,
            HistoryV2InFlightKind::Page {
                direction: HistoryDirection::Before,
                ..
            }
        ));
    }

    // 方向未知（滚动条跳转）同样不主动 seek。
    assert!(!event::request_gap_page_after_scroll(None));
    assert!(rx.try_recv().is_err());

    // 释放认领后，向上滚动才允许以缺口锚点 seek 恢复。
    let handle = crate::kit::atoms::HISTORY_V2_STATE.state();
    handle
        .write()
        .window_mut()
        .expect("window")
        .cancel_request(request_key);
    assert!(event::request_gap_page_after_scroll(Some(
        GapSeekDirection::Before
    )));
    assert!(matches!(
        rx.try_recv(),
        Ok(crate::kit::history_v2::outbound::OutboundCommand::Request(
            crate::kit::history_v2::consumer::HistoryV2ConsumerRequest::Seek(_)
        ))
    ));
}
