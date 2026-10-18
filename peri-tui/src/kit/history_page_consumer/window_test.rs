//! S5.4 consumer 状态机与能力门测试。

use super::*;
use crate::kit::atoms;
use crate::kit::history_paging::{
    WindowBodyDescriptor, WindowDirection, WindowPageItem, WindowPageResponse,
};
use crate::kit::history_window::WindowIdentity;
use serial_test::serial;
use std::time::Instant;

struct WindowAtomsGuard;

impl Drop for WindowAtomsGuard {
    fn drop(&mut self) {
        atoms::HISTORY_WINDOW_V1.set(false);
        atoms::HISTORY_WINDOW_CAPABLE.set(false);
        reset_window_paging_state();
    }
}

fn reset_atoms() -> WindowAtomsGuard {
    crate::kit::atoms::init_atoms();
    atoms::HISTORY_WINDOW_V1.set(true);
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    atoms::HISTORY_WINDOW_GENERATION.set(1);
    reset_window_paging_state();
    note_window_generation(1);
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    state.session_id = "s1".into();
    state.identity = Some(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    state.window_generation = 1;
    state.read_ticket = Some("ticket-1".into());
    state.before_cursor = Some("before-1".into());
    state.after_cursor = Some("after-1".into());
    WindowAtomsGuard
}

fn page_response(snapshot: &str, items: usize) -> WindowPageResponse {
    let item = WindowPageItem {
        source: crate::kit::history_window::HistorySource::Own,
        entry_id: "s1:m1".into(),
        position: Some(1),
        oversize: false,
        sub_items: Vec::new(),
        descriptor: None::<WindowBodyDescriptor>,
        descriptor_only: false,
    };
    WindowPageResponse {
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "gen-1", 7, snapshot),
        committed_tail: None,
        checkpoint_id: None,
        read_ticket: "ticket-1".into(),
        before_cursor: Some("before-2".into()),
        after_cursor: None,
        items: vec![item; items],
        body_descriptors: Vec::new(),
        descriptor_only_items: 0,
        inherited: None,
        inherited_unsupported: false,
        reopen_required: false,
    }
}

fn page_request() -> WindowConsumerRequest {
    build_window_page_request(
        crate::kit::history_paging::WindowPageCursor::Before("before-1".into()),
        20,
    )
    .expect("page request")
}

#[test]
#[serial]
fn newer_page_trigger_stops_at_durable_end_without_repeating_empty_pages() {
    let _guard = reset_atoms();
    {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let mut state = handle.write();
        state.after_cursor = None;
        state.newer_exhausted = false;
    }
    assert!(!request_window_newer_page_if_due());
    atoms::HISTORY_WINDOW_PAGING.state().write().newer_exhausted = true;
    assert!(!request_window_newer_page_if_due());

    let _guard2 = reset_atoms();
    let request = build_window_page_request(
        crate::kit::history_paging::WindowPageCursor::After("after-1".into()),
        20,
    )
    .expect("after request");
    let in_flight = match claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        settle_window_response(&in_flight, &page_response("snap-1", 0)),
        WindowSettlement::AcceptedEmpty
    );
    assert!(atoms::HISTORY_WINDOW_PAGING.state().read().newer_exhausted);
}

#[test]
#[serial]
fn bidirectional_page_requests_keep_placement_and_cursors() {
    let _guard = reset_atoms();
    let before = build_window_page_request(
        crate::kit::history_paging::WindowPageCursor::Before("before-1".into()),
        20,
    )
    .expect("before page");
    match before {
        WindowConsumerRequest::Page {
            context, cursor, ..
        } => {
            assert_eq!(context.placement, WindowPagePlacement::BeforeFirst);
            assert_eq!(
                cursor,
                crate::kit::history_paging::WindowPageCursor::Before("before-1".into())
            );
        }
        other => panic!("expected before page, got {other:?}"),
    }
    let after = build_window_page_request(
        crate::kit::history_paging::WindowPageCursor::After("after-1".into()),
        20,
    )
    .expect("after page");
    match after {
        WindowConsumerRequest::Page {
            context, cursor, ..
        } => {
            assert_eq!(context.placement, WindowPagePlacement::Tail);
            assert_eq!(
                cursor,
                crate::kit::history_paging::WindowPageCursor::After("after-1".into())
            );
        }
        other => panic!("expected after page, got {other:?}"),
    }
}

#[test]
#[serial]
fn window_path_requires_both_switch_and_connection_capability() {
    let _guard = WindowAtomsGuard;
    crate::kit::atoms::init_atoms();
    atoms::HISTORY_WINDOW_V1.set(false);
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    assert!(!window_path_enabled());
    atoms::HISTORY_WINDOW_V1.set(true);
    atoms::HISTORY_WINDOW_CAPABLE.set(false);
    assert!(!window_path_enabled());
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    assert!(window_path_enabled());
}

#[test]
#[serial]
fn one_in_flight_and_one_pending_merge_are_enforced() {
    let _guard = reset_atoms();
    let first = page_request();
    assert!(matches!(
        claim_window_request(&first),
        WindowClaim::Granted(_)
    ));
    assert_eq!(claim_window_request(&first), WindowClaim::Merged);
    let second = build_window_page_request(
        crate::kit::history_paging::WindowPageCursor::After("after-1".into()),
        20,
    )
    .expect("second page");
    assert_eq!(
        claim_window_request(&second),
        WindowClaim::Rejected(WindowClaimRejection::InFlight)
    );

    let in_flight = match claim_window_request(&first) {
        WindowClaim::Merged => atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .in_flight
            .clone()
            .expect("in flight"),
        _ => panic!("expected merged in-flight"),
    };
    assert_eq!(
        in_flight.reservation.bytes,
        in_flight.context.reserved_bytes
    );
    assert_eq!(
        in_flight.reservation.request_id,
        in_flight.context.request_id
    );
    assert!(matches!(
        settle_window_response(&in_flight, &page_response("snap-1", 1)),
        WindowSettlement::Accepted(_)
    ));
    assert!(
        atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .pending_merge
            .is_some()
    );
    let third = build_window_seek_request(
        crate::kit::history_window::PersistentEntryKey::own("s1:m1"),
        10,
        10,
    )
    .expect("seek request");
    assert_eq!(
        claim_window_request(&third),
        WindowClaim::Rejected(WindowClaimRejection::PendingMerge)
    );
    let request_id = atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .pending_merge
        .as_ref()
        .unwrap()
        .context
        .request_id
        .clone();
    assert!(finish_window_merge(&request_id) > 0);
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.reserved_bytes, 0);
    assert!(state.pending_merge.is_none());
}

#[test]
#[serial]
fn stale_or_late_response_is_dropped_and_reservation_released() {
    let _guard = reset_atoms();
    let request = page_request();
    let in_flight = match claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        settle_window_response(&in_flight, &page_response("snap-other", 1)),
        WindowSettlement::Stale
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(state.in_flight.is_none());
    assert_eq!(state.reserved_bytes, 0);

    // 会话边界清空后，旧页连在途身份都不存在。
    reset_window_paging_state();
    assert_eq!(
        settle_window_response(&in_flight, &page_response("snap-1", 1)),
        WindowSettlement::Unmatched
    );
}

#[test]
#[serial]
fn empty_page_releases_reservation_without_pending_merge() {
    let _guard = reset_atoms();
    let request = page_request();
    let in_flight = match claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        settle_window_response(&in_flight, &page_response("snap-1", 0)),
        WindowSettlement::AcceptedEmpty
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.reserved_bytes, 0);
    assert!(state.pending_merge.is_none());
}

#[test]
#[serial]
fn reopen_required_enters_degraded_and_retry_uses_open_not_full_load() {
    let _guard = reset_atoms();
    let request = page_request();
    let in_flight = match claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    let mut response = page_response("snap-1", 1);
    response.reopen_required = true;
    assert_eq!(
        settle_window_response(&in_flight, &response),
        WindowSettlement::ReopenRequired
    );
    {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let mut state = handle.write();
        assert!(state.is_degraded());
        state.status = WindowPagingStatus::Degraded {
            reason: "reopen".into(),
            retry_at: Some(Instant::now() - std::time::Duration::from_millis(1)),
            attempts: 1,
        };
    }
    let retry = retry_window_open_if_due(Instant::now()).expect("retry request");
    assert!(matches!(retry, WindowConsumerRequest::Open { .. }));
    assert_eq!(retry.session_id(), "s1");
    let in_flight = match claim_window_request(&retry) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("degraded open retry must be claimable, got {other:?}"),
    };
    assert!(matches!(
        settle_window_response(&in_flight, &page_response("snap-reopen", 1)),
        WindowSettlement::Accepted(_)
    ));
    assert!(!atoms::HISTORY_WINDOW_PAGING.state().read().is_degraded());
}

#[test]
#[serial]
fn invalidating_failure_degrades_and_non_invalidating_failure_keeps_window() {
    let _guard = reset_atoms();
    let first = page_request();
    let in_flight = match claim_window_request(&first) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    fail_window_request(&in_flight.context.request_id, "bad cursor", true);
    assert!(atoms::HISTORY_WINDOW_PAGING.state().read().is_degraded());
    assert_eq!(
        atoms::HISTORY_WINDOW_PAGING.state().read().reserved_bytes,
        0
    );

    let _guard = reset_atoms();
    let second = page_request();
    let in_flight = match claim_window_request(&second) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    fail_window_request(&in_flight.context.request_id, "timeout", false);
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(!state.is_degraded());
    assert_eq!(state.last_error.as_deref(), Some("timeout"));
}

#[test]
#[serial]
fn gap_request_uses_stable_identity_seek_and_keeps_window_open() {
    use crate::kit::history_window::{
        HistoryEntryView, HistorySource, HistoryWindow, HistoryWindowConfig, PersistentEntryKey,
        ReleasePlan, SegmentDraft, SegmentPlacement, WindowAnchor, WindowAnchorOffset,
    };
    let _guard = reset_atoms();
    let identity = WindowIdentity::new("s1", "gen-1", 7, "snap-1");
    let mut window = HistoryWindow::new(HistoryWindowConfig::enabled());
    window.begin_window(identity.clone());
    let mut keys = Vec::new();
    for (id, position) in [("a", 10), ("b", 20), ("c", 30)] {
        let entry = HistoryEntryView::new(
            PersistentEntryKey::own(format!("s1:{id}")),
            Some(position),
            1,
            10,
            20,
            Vec::new(),
        );
        keys.push(
            window
                .register_segment(
                    SegmentDraft::new(HistorySource::Own, vec![entry], 10, 20),
                    SegmentPlacement::Tail,
                )
                .expect("segment"),
        );
    }
    let b_entry = window.segment(keys[1]).unwrap().entries[0].key.clone();
    let (before, after) = window.neighbor_entries(keys[1]);
    let segment = window.segment(keys[1]).unwrap().clone();
    let plan = ReleasePlan {
        segment: keys[1],
        source: segment.source.clone(),
        entries: segment.entry_keys(),
        unit_count: segment.unit_count,
        body_bytes: segment.body_bytes,
        reserved_bytes: segment.reserved_bytes,
        slot_range: window.index().slot_range(keys[1]),
        before,
        after,
        before_cursor: Some("before-b".into()),
        after_cursor: Some("after-b".into()),
    };
    window.apply_release_plan(&plan).expect("release b");
    window.set_anchor(Some(WindowAnchor::new(
        b_entry.clone(),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));

    let request = build_window_gap_request(&window).expect("gap request");
    match request {
        WindowConsumerRequest::Seek {
            context, anchor, ..
        } => {
            assert_eq!(anchor, b_entry);
            assert_eq!(context.session_id, "s1");
            assert_eq!(context.identity.snapshot_token, "snap-1");
        }
        other => panic!("expected seek request, got {other:?}"),
    }
}

#[test]
#[serial]
fn page_settlement_keeps_the_opposite_window_cursor() {
    {
        let _guard = reset_atoms();
        let request = build_window_page_request(
            crate::kit::history_paging::WindowPageCursor::Before("before-1".into()),
            20,
        )
        .expect("before page");
        let in_flight = match claim_window_request(&request) {
            WindowClaim::Granted(in_flight) => in_flight,
            other => panic!("expected grant, got {other:?}"),
        };
        let mut response = page_response("snap-1", 1);
        response.before_cursor = Some("before-2".into());
        response.after_cursor = None;
        assert!(matches!(
            settle_window_response(&in_flight, &response),
            WindowSettlement::Accepted(_)
        ));
        let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
        assert_eq!(state.before_cursor.as_deref(), Some("before-2"));
        assert_eq!(
            state.after_cursor.as_deref(),
            Some("after-1"),
            "a before page must not clear the newer-side cursor"
        );
    }

    {
        let _guard = reset_atoms();
        let request = build_window_page_request(
            crate::kit::history_paging::WindowPageCursor::After("after-1".into()),
            20,
        )
        .expect("after page");
        let in_flight = match claim_window_request(&request) {
            WindowClaim::Granted(in_flight) => in_flight,
            other => panic!("expected grant, got {other:?}"),
        };
        let mut response = page_response("snap-1", 1);
        response.before_cursor = None;
        response.after_cursor = Some("after-2".into());
        assert!(matches!(
            settle_window_response(&in_flight, &response),
            WindowSettlement::Accepted(_)
        ));
        let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
        assert_eq!(
            state.before_cursor.as_deref(),
            Some("before-1"),
            "an after page must not clear the older-side cursor"
        );
        assert_eq!(state.after_cursor.as_deref(), Some("after-2"));
    }
}

#[test]
#[serial]
fn gap_request_carries_gap_identity_and_window_generation() {
    use crate::kit::history_window::{
        HistoryEntryView, HistorySource, HistoryWindow, HistoryWindowConfig, PersistentEntryKey,
        ReleasePlan, SegmentDraft, SegmentPlacement,
    };

    let _guard = reset_atoms();
    let identity = WindowIdentity::new("s1", "gen-1", 7, "snap-1");
    let mut window = HistoryWindow::new(HistoryWindowConfig::enabled());
    window.begin_window(identity);
    let mut keys = Vec::new();
    for (id, position) in [("a", 10), ("b", 20)] {
        let entry = HistoryEntryView::new(
            PersistentEntryKey::own(format!("s1:{id}")),
            Some(position),
            1,
            10,
            20,
            Vec::new(),
        );
        keys.push(
            window
                .register_segment(
                    SegmentDraft::new(HistorySource::Own, vec![entry], 10, 20),
                    SegmentPlacement::Tail,
                )
                .expect("segment"),
        );
    }
    let (before, after) = window.neighbor_entries(keys[1]);
    let segment = window.segment(keys[1]).expect("segment").clone();
    window
        .apply_release_plan(&ReleasePlan {
            segment: keys[1],
            source: segment.source.clone(),
            entries: segment.entry_keys(),
            unit_count: segment.unit_count,
            body_bytes: segment.body_bytes,
            reserved_bytes: segment.reserved_bytes,
            slot_range: window.index().slot_range(keys[1]),
            before,
            after,
            before_cursor: Some("before-b".into()),
            after_cursor: Some("after-b".into()),
        })
        .expect("release b");
    let anchor = crate::kit::history_window::WindowAnchor::new(
        PersistentEntryKey::own("s1:b"),
        None,
        crate::kit::history_window::WindowAnchorOffset::CharOffset(0),
    );
    window.set_anchor(Some(anchor));

    let request = build_window_gap_request(&window).expect("gap seek request");
    let gap_id = *window.gaps().keys().next().expect("gap");
    match request {
        WindowConsumerRequest::Seek { context, .. } => {
            assert_eq!(context.gap, Some(gap_id));
            assert_eq!(
                context.display_generation,
                atoms::VIEW_MODELS.state().read().generation
            );
        }
        other => panic!("expected seek, got {other:?}"),
    }
}

#[test]
#[serial]
fn reservation_budget_rejects_before_request_dispatch() {
    let _guard = reset_atoms();
    let identity = WindowIdentity::new("s1", "gen-1", 7, "snap-1");
    let request = WindowConsumerRequest::Page {
        context: WindowRequestContext {
            request_id: "too-big".into(),
            session_id: "s1".into(),
            identity,
            window_generation: 1,
            reserved_bytes: crate::kit::history_window::WindowLimits::default().reserved_bytes + 1,
            placement: WindowPagePlacement::Tail,
            is_open: false,
            display_generation: 0,
            gap: None,
        },
        cursor: crate::kit::history_paging::WindowPageCursor::Before("c".into()),
        limit: 20,
    };
    assert!(matches!(
        claim_window_request(&request),
        WindowClaim::Rejected(WindowClaimRejection::ReservationBudget { .. })
    ));
}

#[test]
#[serial]
fn open_request_skips_prior_snapshot_validation() {
    let _guard = reset_atoms();
    let request = build_window_open_request(WindowDirection::Before);
    let in_flight = match claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        settle_window_response(&in_flight, &page_response("new-snap", 1)),
        WindowSettlement::Accepted(Box::new(in_flight))
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.identity.unwrap().snapshot_token, "new-snap");
}
