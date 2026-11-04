//! Bridge reset 必须同时清空 S5 历史窗口模型（会话边界清理）。

use super::*;
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, HistoryWindow, HistoryWindowConfig, PersistentEntryKey,
    SegmentDraft, SegmentPlacement, WindowIdentity,
};

#[test]
#[serial]
fn test_bridge_reset_clears_history_window() {
    let old_reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let old_active = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let mut state = scheduler_state();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 1, "snap-1"));
    state
        .window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(
                    PersistentEntryKey::own("s1:m1"),
                    Some(1),
                    1,
                    10,
                    20,
                    Vec::new(),
                )],
                10,
                20,
            ),
            SegmentPlacement::Tail,
        )
        .expect("segment");
    assert_eq!(state.window.segment_count(), 1);

    let mut last_reset = old_reset;
    apply_bridge_reset(&mut state, &mut last_reset, old_reset.wrapping_add(1));

    assert_eq!(state.window.segment_count(), 0);
    assert!(state.window.identity().is_none());
    assert_eq!(state.window.budget().resident_body_bytes, 0);
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = old_active;
}

/// 会话边界先同步清空 consumer 状态，bridge 稍后才观察到同一个 reset 计数器。
/// 这次迟到的 reset 不得再清一次 consumer 状态：新会话的首个 `history.open`
/// 可能已经在途，清掉后响应会被判过期，首屏一直为空。
#[test]
#[serial]
fn delayed_bridge_reset_keeps_new_session_first_open_in_flight() {
    use crate::kit::history_page_consumer::{
        WindowClaim, WindowSettlement, build_window_open_request, claim_window_request,
        reset_window_paging_state, settle_window_response,
    };
    use crate::kit::history_paging::{WindowDirection, WindowPageResponse};

    let old_reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let old_active = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let mut state = scheduler_state();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(true);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(true);
    let session = "s-first-open";
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = session.to_owned();
    reset_window_paging_state();
    let request = {
        let handle = crate::kit::atoms::HISTORY_WINDOW_PAGING.state();
        let mut paging = handle.write();
        paging.session_id = session.to_owned();
        drop(paging);
        build_window_open_request(WindowDirection::Before)
    };
    let in_flight = match claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected open grant, got {other:?}"),
    };

    let mut last_reset = old_reset;
    apply_bridge_reset(&mut state, &mut last_reset, old_reset.wrapping_add(1));

    assert!(
        crate::kit::atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .in_flight
            .is_some(),
        "迟到的 bridge reset 不得清掉新会话的首个 open"
    );
    let response = WindowPageResponse {
        session_id: session.to_owned(),
        identity: WindowIdentity::new(session, "gen-1", 1, "snap-1"),
        committed_tail: None,
        checkpoint_id: None,
        read_ticket: "ticket-1".into(),
        before_cursor: None,
        after_cursor: None,
        items: Vec::new(),
        body_descriptors: Vec::new(),
        descriptor_only_items: 0,
        inherited: None,
        inherited_unsupported: false,
        reopen_required: false,
    };
    assert!(matches!(
        settle_window_response(&in_flight, &response),
        WindowSettlement::AcceptedEmpty
    ));
    reset_window_paging_state();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = old_active;
}
