//! S5.4 consumer 状态机与能力门测试。

use super::WindowClaim;
use super::WindowClaimRejection;
use super::WindowConsumerRequest;
use super::WindowInFlight;
use super::WindowPagePlacement;
use super::WindowPagingStatus;
use super::WindowRequestContext;
use super::WindowSettlement;

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
        super::reset_window_paging_state();
    }
}

fn reset_atoms() -> WindowAtomsGuard {
    crate::kit::atoms::init_atoms();
    atoms::HISTORY_WINDOW_V1.set(true);
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    atoms::HISTORY_WINDOW_GENERATION.set(1);
    super::reset_window_paging_state();
    super::note_window_generation(1);
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
    super::build_window_page_request(
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
    let request = super::build_window_page_request(
        crate::kit::history_paging::WindowPageCursor::After("after-1".into()),
        20,
    )
    .expect("after request");
    let in_flight = match super::claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        super::settle_window_response(&in_flight, &page_response("snap-1", 0)),
        WindowSettlement::AcceptedEmpty
    );
    assert!(atoms::HISTORY_WINDOW_PAGING.state().read().newer_exhausted);
}

#[test]
#[serial]
fn bidirectional_page_requests_keep_placement_and_cursors() {
    let _guard = reset_atoms();
    let before = super::build_window_page_request(
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
    let after = super::build_window_page_request(
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
    assert!(!super::window_path_enabled());
    atoms::HISTORY_WINDOW_V1.set(true);
    atoms::HISTORY_WINDOW_CAPABLE.set(false);
    assert!(!super::window_path_enabled());
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    assert!(super::window_path_enabled());
}

#[test]
#[serial]
fn one_in_flight_and_one_pending_merge_are_enforced() {
    let _guard = reset_atoms();
    let first = page_request();
    assert!(matches!(
        super::claim_window_request(&first),
        WindowClaim::Granted(_)
    ));
    assert_eq!(super::claim_window_request(&first), WindowClaim::Merged);
    let second = super::build_window_page_request(
        crate::kit::history_paging::WindowPageCursor::After("after-1".into()),
        20,
    )
    .expect("second page");
    assert_eq!(
        super::claim_window_request(&second),
        WindowClaim::Rejected(WindowClaimRejection::InFlight)
    );

    let in_flight = match super::claim_window_request(&first) {
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
        super::settle_window_response(&in_flight, &page_response("snap-1", 1)),
        WindowSettlement::Accepted(_)
    ));
    assert!(
        atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .pending_merge
            .is_some()
    );
    let third = super::build_window_seek_request(
        crate::kit::history_window::PersistentEntryKey::own("s1:m1"),
        10,
        10,
    )
    .expect("seek request");
    assert_eq!(
        super::claim_window_request(&third),
        WindowClaim::Rejected(WindowClaimRejection::PendingMerge)
    );
    let request_id = atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .pending_merge
        .as_ref()
        .expect("one in flight and one pending merge are enforced: isolated fixture workspace must remain accessible")
        .context
        .request_id
        .clone();
    assert!(super::finish_window_merge(&request_id) > 0);
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.reserved_bytes, 0);
    assert!(state.pending_merge.is_none());
}

#[test]
#[serial]
fn stale_or_late_response_is_dropped_and_reservation_released() {
    let _guard = reset_atoms();
    let request = page_request();
    let in_flight = match super::claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        super::settle_window_response(&in_flight, &page_response("snap-other", 1)),
        WindowSettlement::Stale
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(state.in_flight.is_none());
    assert_eq!(state.reserved_bytes, 0);

    // 会话边界清空后，旧页连在途身份都不存在。
    super::reset_window_paging_state();
    assert_eq!(
        super::settle_window_response(&in_flight, &page_response("snap-1", 1)),
        WindowSettlement::Unmatched
    );
}

#[test]
#[serial]
fn stale_response_from_another_session_never_restores_window_state() {
    let _guard = reset_atoms();
    let old_request = super::build_window_open_request(WindowDirection::Before);
    let old_in_flight = match super::claim_window_request(&old_request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected old open grant, got {other:?}"),
    };
    // 会话边界同步清空后，新会话的 open 使用新的请求序号。
    super::reset_window_paging_state();
    {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let mut state = handle.write();
        state.session_id = "s2".into();
    }
    let new_request = super::build_window_open_request(WindowDirection::Before);
    assert!(
        matches!(
            super::claim_window_request(&new_request),
            WindowClaim::Granted(_)
        ),
        "new session open must be in flight"
    );

    // 旧会话 s1 的响应迟到：请求序号不再复用，既不匹配新请求，也不得清掉新槽位。
    let stale = page_response("snap-old", 1);
    assert_eq!(
        super::settle_window_response(&old_in_flight, &stale),
        WindowSettlement::Unmatched
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.session_id, "s2");
    assert!(state.identity.is_none());
    assert!(state.read_ticket.is_none());
    assert!(state.before_cursor.is_none());
    assert!(state.after_cursor.is_none());
    assert!(state.in_flight.is_some(), "new session open must survive");
    assert!(state.reserved_bytes > 0);
}

#[test]
#[serial]
fn empty_page_releases_reservation_without_pending_merge() {
    let _guard = reset_atoms();
    let request = page_request();
    let in_flight = match super::claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        super::settle_window_response(&in_flight, &page_response("snap-1", 0)),
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
    let in_flight = match super::claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    let mut response = page_response("snap-1", 1);
    response.reopen_required = true;
    assert_eq!(
        super::settle_window_response(&in_flight, &response),
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
    let in_flight = match super::claim_window_request(&retry) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("degraded open retry must be claimable, got {other:?}"),
    };
    assert!(matches!(
        super::settle_window_response(&in_flight, &page_response("snap-reopen", 1)),
        WindowSettlement::Accepted(_)
    ));
    assert!(!atoms::HISTORY_WINDOW_PAGING.state().read().is_degraded());
}

#[test]
#[serial]
fn invalidating_failure_degrades_and_non_invalidating_failure_keeps_window() {
    let _guard = reset_atoms();
    let first = page_request();
    let in_flight = match super::claim_window_request(&first) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    super::fail_window_request(&in_flight.context.request_id, "bad cursor", true);
    assert!(atoms::HISTORY_WINDOW_PAGING.state().read().is_degraded());
    assert_eq!(
        atoms::HISTORY_WINDOW_PAGING.state().read().reserved_bytes,
        0
    );

    let _guard = reset_atoms();
    let second = page_request();
    let in_flight = match super::claim_window_request(&second) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    super::fail_window_request(&in_flight.context.request_id, "timeout", false);
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
    let b_entry = window.segment(keys[1]).expect("gap request uses stable identity seek and keeps window open: the segment fixture operation uses valid inputs").entries[0].key.clone();
    let (before, after) = window.neighbor_entries(keys[1]);
    let segment = window.segment(keys[1]).expect("gap request uses stable identity seek and keeps window open: the segment fixture operation uses valid inputs").clone();
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
fn publication_only_advances_the_requested_window_cursor() {
    {
        let _guard = reset_atoms();
        let request = super::build_window_page_request(
            crate::kit::history_paging::WindowPageCursor::Before("before-1".into()),
            20,
        )
        .expect("before page");
        let in_flight = match super::claim_window_request(&request) {
            WindowClaim::Granted(in_flight) => in_flight,
            other => panic!("expected grant, got {other:?}"),
        };
        let mut response = page_response("snap-1", 1);
        response.before_cursor = Some("before-2".into());
        response.after_cursor = Some("after-2".into());
        assert!(matches!(
            super::settle_window_response(&in_flight, &response),
            WindowSettlement::Accepted(_)
        ));
        let unsettled = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
        assert_eq!(unsettled.before_cursor.as_deref(), Some("before-1"));
        assert_eq!(unsettled.after_cursor.as_deref(), Some("after-1"));
        assert!(super::finish_window_merge(&in_flight.context.request_id) > 0);
        let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
        assert_eq!(state.before_cursor.as_deref(), Some("before-2"));
        assert_eq!(
            state.after_cursor.as_deref(),
            Some("after-1"),
            "a before page describes its own newest row, not the window's newer edge"
        );
    }

    {
        let _guard = reset_atoms();
        let request = super::build_window_page_request(
            crate::kit::history_paging::WindowPageCursor::After("after-1".into()),
            20,
        )
        .expect("after page");
        let in_flight = match super::claim_window_request(&request) {
            WindowClaim::Granted(in_flight) => in_flight,
            other => panic!("expected grant, got {other:?}"),
        };
        let mut response = page_response("snap-1", 1);
        response.before_cursor = Some("before-2".into());
        response.after_cursor = Some("after-2".into());
        assert!(matches!(
            super::settle_window_response(&in_flight, &response),
            WindowSettlement::Accepted(_)
        ));
        let unsettled = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
        assert_eq!(unsettled.before_cursor.as_deref(), Some("before-1"));
        assert_eq!(unsettled.after_cursor.as_deref(), Some("after-1"));
        assert!(super::finish_window_merge(&in_flight.context.request_id) > 0);
        let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
        assert_eq!(
            state.before_cursor.as_deref(),
            Some("before-1"),
            "an after page describes its own oldest row, not the window's older edge"
        );
        assert_eq!(state.after_cursor.as_deref(), Some("after-2"));
    }
}

#[test]
#[serial]
fn discarded_pending_page_cannot_spend_the_old_boundary_or_commit_into_a_new_session() {
    let _guard = reset_atoms();
    let old = granted(&page_request());
    let mut page = page_response("snap-1", 1);
    page.read_ticket = "ticket-that-was-rejected".into();
    assert!(matches!(
        super::settle_window_response(&old, &page),
        WindowSettlement::Accepted(_)
    ));
    super::note_window_cursors(
        Some(&old.context.request_id),
        Some("new-before".into()),
        Some("new-after".into()),
    );
    assert_eq!(super::finish_window_merge("unrelated"), 0);
    super::fail_window_request(&old.context.request_id, "waiting for render ACK", false);
    let paging = atoms::HISTORY_WINDOW_PAGING.state();
    let untouched = paging.read().clone();
    assert_eq!(untouched.before_cursor.as_deref(), Some("before-1"));
    assert_eq!(untouched.after_cursor.as_deref(), Some("after-1"));
    assert_eq!(untouched.read_ticket.as_deref(), Some("ticket-1"));
    assert_eq!(untouched.reserved_bytes, 0);
    let retry = granted(&page_request());
    assert_eq!(super::finish_window_merge(&old.context.request_id), 0);
    assert!(matches!(
        super::settle_window_response(&retry, &page),
        WindowSettlement::Accepted(_)
    ));

    switch_window_session("s2");
    let new = granted(&super::build_window_open_request(WindowDirection::Before));
    super::note_window_cursors(
        Some(&retry.context.request_id),
        Some("late-before".into()),
        None,
    );
    super::fail_window_request(&retry.context.request_id, "late rejection", true);
    assert_eq!(super::finish_window_merge(&retry.context.request_id), 0);
    let current = paging.read();
    assert_eq!(current.in_flight.as_ref(), Some(&new));
    assert!(!current.is_degraded());
    assert!(current.before_cursor.is_none());
    assert!(current.after_cursor.is_none());
    assert!(current.read_ticket.is_none());
    assert_eq!(current.reserved_bytes, new.reservation.bytes);
}

#[test]
#[serial]
fn note_window_cursors_recomputes_both_edges_and_reopens_newer_direction() {
    let _guard = reset_atoms();
    atoms::HISTORY_WINDOW_PAGING.state().write().newer_exhausted = true;
    super::note_window_cursors(None, Some("before-edge".into()), Some("after-edge".into()));
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.before_cursor.as_deref(), Some("before-edge"));
    assert_eq!(state.after_cursor.as_deref(), Some("after-edge"));
    assert!(
        !state.newer_exhausted,
        "an available newer cursor reopens the newer direction"
    );
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
        super::claim_window_request(&request),
        WindowClaim::Rejected(WindowClaimRejection::ReservationBudget { .. })
    ));
}

#[test]
#[serial]
fn open_request_skips_prior_snapshot_validation() {
    let _guard = reset_atoms();
    let request = super::build_window_open_request(WindowDirection::Before);
    let in_flight = match super::claim_window_request(&request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    };
    assert_eq!(
        super::settle_window_response(&in_flight, &page_response("new-snap", 1)),
        WindowSettlement::Accepted(Box::new(in_flight))
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.identity.expect("open request skips prior snapshot validation: the fixture establishes state.identity before this assertion").snapshot_token, "new-snap");
}

fn session_response(session: &str, snapshot: &str, items: usize) -> WindowPageResponse {
    let mut response = page_response(snapshot, items);
    response.session_id = session.into();
    response.identity = WindowIdentity::new(session, "gen-1", 7, snapshot);
    response
}

fn switch_window_session(session_id: &str) {
    super::reset_window_paging_state();
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    state.session_id = session_id.to_string();
}

fn granted(request: &WindowConsumerRequest) -> WindowInFlight {
    match super::claim_window_request(request) {
        WindowClaim::Granted(in_flight) => in_flight,
        other => panic!("expected grant, got {other:?}"),
    }
}

fn open_request_id(request: &WindowConsumerRequest) -> String {
    match request {
        WindowConsumerRequest::Open { request_id, .. } => request_id.clone(),
        other => panic!("expected open request, got {other:?}"),
    }
}

/// 会话边界后仍在队列里的旧 open 不得占用新窗口的在途槽位。
#[test]
#[serial]
fn queued_open_from_previous_window_is_rejected_after_session_switch() {
    let _guard = reset_atoms();
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    handle.write().session_id = "s2".into();
    let stale_open = super::build_window_open_request(WindowDirection::Before);
    switch_window_session("s3");

    assert_eq!(
        super::claim_window_request(&stale_open),
        WindowClaim::Rejected(WindowClaimRejection::SessionMismatch {
            requested: "s2".into(),
            current: "s3".into(),
        })
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(state.in_flight.is_none());
    assert_eq!(state.reserved_bytes, 0);
}

/// A open 在途 → B open → C open：B 的非空回包不得进入 C 的 `pending_merge`，
/// 也不得清掉 C 自己的在途槽位或写入 B 的身份。
#[test]
#[serial]
fn late_open_response_from_previous_window_never_merges_into_current_window() {
    let _guard = reset_atoms();
    let in_flight_a = granted(&super::build_window_open_request(WindowDirection::Before));

    switch_window_session("s2");
    let in_flight_b = granted(&super::build_window_open_request(WindowDirection::Before));

    switch_window_session("s3");
    let in_flight_c = granted(&super::build_window_open_request(WindowDirection::Before));

    // A 与 B 的响应都在会话切走后到达；两者都不能命中 C 的在途槽位。
    assert_eq!(
        super::settle_window_response(&in_flight_a, &session_response("s1", "snap-a", 1)),
        WindowSettlement::Unmatched
    );
    assert_eq!(
        super::settle_window_response(&in_flight_b, &session_response("s2", "snap-b", 1)),
        WindowSettlement::Unmatched
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(state.session_id, "s3");
    assert!(state.pending_merge.is_none());
    assert!(state.identity.is_none());
    assert_eq!(
        state
            .in_flight
            .as_ref()
            .map(|in_flight| { in_flight.context.request_id.as_str() }),
        Some(in_flight_c.context.request_id.as_str())
    );
    assert_eq!(state.reserved_bytes, in_flight_c.reservation.bytes);

    // C 自己的响应仍正常结算，并成为唯一的 pending_merge。
    assert!(matches!(
        super::settle_window_response(&in_flight_c, &session_response("s3", "snap-c", 1)),
        WindowSettlement::Accepted(_)
    ));
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert_eq!(
        state
            .pending_merge
            .as_ref()
            .map(|pending| pending.context.session_id.as_str()),
        Some("s3")
    );
    assert_eq!(state.identity.expect("late open response from previous window never merges into current window: the fixture establishes state.identity before this assertion").snapshot_token, "snap-c");
}

/// 当前窗口身份是结算权威：在途槽位若残留旧窗口上下文（防线性场景），
/// 迟到响应必须判定 Stale 并释放槽位，不得以旧会话身份进入 `pending_merge`。
#[test]
#[serial]
fn settle_treats_in_flight_from_another_window_as_stale() {
    let _guard = reset_atoms();
    let in_flight = granted(&super::build_window_open_request(WindowDirection::Before));
    {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let mut state = handle.write();
        state.session_id = "s2".into();
        state.identity = None;
    }

    assert_eq!(
        super::settle_window_response(&in_flight, &session_response("s1", "snap-old", 1)),
        WindowSettlement::Stale
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(state.in_flight.is_none());
    assert!(state.pending_merge.is_none());
    assert!(state.identity.is_none());
    assert_eq!(state.reserved_bytes, 0);
    assert_eq!(state.session_id, "s2");
}

/// seek 结算同样以当前窗口代次为权威：代次推进后的旧 seek 响应不得合并。
#[test]
#[serial]
fn seek_settlement_uses_current_window_generation() {
    let _guard = reset_atoms();
    let seek = super::build_window_seek_request(
        crate::kit::history_window::PersistentEntryKey::own("s1:m1"),
        10,
        10,
    )
    .expect("seek request");
    let in_flight = granted(&seek);
    super::note_window_generation(in_flight.context.window_generation + 1);

    assert_eq!(
        super::settle_window_response(&in_flight, &session_response("s1", "snap-1", 1)),
        WindowSettlement::Stale
    );
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(state.in_flight.is_none());
    assert!(state.pending_merge.is_none());
    assert_eq!(state.reserved_bytes, 0);
}

/// 请求序号跨会话边界继续单调，旧响应与新请求不会共享同一个 id。
#[test]
#[serial]
fn request_ids_are_not_reused_across_session_resets() {
    let _guard = reset_atoms();
    let first = super::build_window_open_request(WindowDirection::Before);
    switch_window_session("s2");
    let second = super::build_window_open_request(WindowDirection::Before);
    assert_ne!(open_request_id(&first), open_request_id(&second));
}

/// 旧请求的失败/超时不得把新窗口置为 Degraded，也不得改写其错误状态。
#[test]
#[serial]
fn failure_for_unknown_request_does_not_touch_current_window() {
    let _guard = reset_atoms();
    let request = page_request();
    let in_flight = granted(&request);
    super::fail_window_request("window-retired", "late timeout", true);

    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(!state.is_degraded());
    assert!(state.last_error.is_none());
    assert_eq!(
        state
            .in_flight
            .as_ref()
            .map(|current| current.context.request_id.as_str()),
        Some(in_flight.context.request_id.as_str())
    );
    assert_eq!(state.reserved_bytes, in_flight.reservation.bytes);

    // 命中当前请求的失效错误仍应进入 Degraded。
    super::fail_window_request(&in_flight.context.request_id, "bad cursor", true);
    assert!(atoms::HISTORY_WINDOW_PAGING.state().read().is_degraded());
}

/// 桥接丢弃页事件时必须归还该请求的预留（含 `pending_merge`）。
#[test]
#[serial]
fn abandoned_page_request_releases_pending_merge_reservation() {
    let _guard = reset_atoms();
    let in_flight = granted(&super::build_window_open_request(WindowDirection::Before));
    assert!(matches!(
        super::settle_window_response(&in_flight, &session_response("s1", "snap-1", 1)),
        WindowSettlement::Accepted(_)
    ));
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(state.pending_merge.is_some());
    assert!(state.reserved_bytes > 0);

    assert!(super::abandon_window_request(&in_flight.context.request_id) > 0);
    let state = atoms::HISTORY_WINDOW_PAGING.state().read().clone();
    assert!(state.pending_merge.is_none());
    assert!(state.in_flight.is_none());
    assert_eq!(state.reserved_bytes, 0);
}

use super::window::{
    build_window_gap_request, request_window_newer_page_if_due, retry_window_open_if_due,
};
