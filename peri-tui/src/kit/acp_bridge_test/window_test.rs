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

/// 会话过滤丢弃旧会话的页事件后，consumer 的 `pending_merge` 预留必须释放；
/// 否则当前窗口的 open 会一直被 PendingMerge 拒绝。
#[tokio::test]
#[serial]
async fn dropped_page_event_from_previous_session_releases_consumer_reservation() {
    use crate::kit::atoms::{
        ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, HISTORY_WINDOW_CAPABLE, HISTORY_WINDOW_PAGING,
        HISTORY_WINDOW_V1,
    };
    use crate::kit::history_page_consumer::{
        WindowClaim, WindowSettlement, build_window_open_request, claim_window_request,
        reset_window_paging_state, settle_window_response,
    };
    use crate::kit::history_paging::{
        WindowBodyDescriptor, WindowDirection, WindowPageItem, WindowPageResponse,
    };
    use crate::kit::history_window::{HistorySource, WindowIdentity};

    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_v1 = HISTORY_WINDOW_V1.get();
    let old_capable = HISTORY_WINDOW_CAPABLE.get();
    crate::kit::atoms::init_atoms();
    HISTORY_WINDOW_V1.set(true);
    HISTORY_WINDOW_CAPABLE.set(true);
    reset_window_paging_state();
    HISTORY_WINDOW_PAGING.state().write().session_id = "s-b".into();
    let request = build_window_open_request(WindowDirection::Before);
    let in_flight = match claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected open grant, got {other:?}"),
    };
    let response = WindowPageResponse {
        session_id: "s-b".into(),
        identity: WindowIdentity::new("s-b", "gen-b", 1, "snap-b"),
        committed_tail: None,
        checkpoint_id: None,
        read_ticket: "ticket-b".into(),
        before_cursor: None,
        after_cursor: None,
        items: vec![WindowPageItem {
            source: HistorySource::Own,
            entry_id: "s-b:m1".into(),
            position: Some(1),
            oversize: false,
            sub_items: Vec::new(),
            descriptor: None::<WindowBodyDescriptor>,
            descriptor_only: false,
        }],
        body_descriptors: Vec::new(),
        descriptor_only_items: 0,
        inherited: None,
        inherited_unsupported: false,
        reopen_required: false,
    };
    assert!(matches!(
        settle_window_response(&in_flight, &response),
        WindowSettlement::Accepted(_)
    ));
    let context = HISTORY_WINDOW_PAGING
        .state()
        .read()
        .pending_merge
        .clone()
        .expect("pending merge")
        .context;
    assert!(HISTORY_WINDOW_PAGING.state().read().reserved_bytes > 0);

    // 模拟并发切换：bridge 已指向 s-c，consumer 侧的 s-b 预留尚未结算。
    *ACTIVE_SESSION_ID.state().write() = "s-c".into();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(1).max(1));

    let (tx, rx) = mpsc::channel(crate::kit::acp_bridge::BRIDGE_QUEUE_CAPACITY);
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);
    tx.send(AcpEventWithEpoch {
        child_origin: None,
        view_epoch: BRIDGE_RESET_COUNTER.get(),
        event: AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
        active_session_id: "s-b".into(),
    })
    .await
    .unwrap();

    assert_eq!(observed_rx.recv().await, Some(false));
    let paging = HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(paging.pending_merge.is_none());
    assert!(paging.in_flight.is_none());
    assert_eq!(paging.reserved_bytes, 0);

    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
    HISTORY_WINDOW_V1.set(old_v1);
    HISTORY_WINDOW_CAPABLE.set(old_capable);
    *ACTIVE_SESSION_ID.state().write() = old_active;
    BRIDGE_RESET_COUNTER.set(old_reset);
    reset_window_paging_state();
}
