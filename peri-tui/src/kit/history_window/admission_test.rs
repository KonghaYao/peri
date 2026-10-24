use super::test_support::*;
use super::*;

fn small_limits(body_bytes: usize) -> WindowLimits {
    WindowLimits {
        body_bytes,
        reserved_bytes: 16 * 1024,
        ..WindowLimits::default()
    }
}

#[test]
fn page_reservation_counts_reserved_not_body_and_settles_into_actual_bytes() {
    let mut window = enabled_window(small_limits(10_000));
    assert!(matches!(
        window.reserve_page("r1", 800),
        ReserveOutcome::Granted(_)
    ));
    assert_eq!(window.budget().page_reserved_bytes, 800);
    assert_eq!(window.budget().resident_body_bytes, 0);

    let decision = window.admit_page(PageDraft {
        request_id: "r1".into(),
        window_generation: window.generation(),
        identity: identity(),
        placement: PagePlacement::Tail,
        segments: vec![segment("a", 10, 300, 500)],
    });
    assert!(matches!(decision, AdmissionDecision::Accepted { .. }));
    assert_eq!(window.budget().page_reserved_bytes, 0);
    assert_eq!(window.budget().resident_body_bytes, 300);
    assert_eq!(window.budget().resident_reserved_bytes, 500);
    assert_eq!(window.pending_merge_bytes(), 300);
    assert_eq!(window.settle_pending_merge(), 300);
    assert_eq!(window.pending_merge_bytes(), 0);
}

#[test]
fn over_budget_admission_evicts_farthest_segment_first_and_reports_plans() {
    let mut window = enabled_window(small_limits(1_000));
    let a = accepted_key(admit_and_settle(
        &mut window,
        "r1",
        segment("a", 10, 300, 400),
    ));
    let b = accepted_key(admit_and_settle(
        &mut window,
        "r2",
        segment("b", 20, 300, 400),
    ));
    let c = accepted_key(admit_and_settle(
        &mut window,
        "r3",
        segment("c", 30, 300, 400),
    ));
    assert_eq!(window.budget().resident_body_bytes, 900);
    assert_eq!(window.generation(), 1);

    let anchor_entry = window.segment(a).unwrap().entries[0].key.clone();
    assert!(matches!(
        window.set_anchor(Some(WindowAnchor::new(
            anchor_entry,
            None,
            WindowAnchorOffset::CharOffset(0),
        ))),
        AnchorRelocation::Resident { segment, .. } if segment == a
    ));

    let decision = admit(
        &mut window,
        "r4",
        segment("d", 40, 300, 400),
        PagePlacement::Tail,
    );
    let (segments, released) = match decision {
        AdmissionDecision::Accepted {
            segments, released, ..
        } => (segments, released),
        other => panic!("expected eviction + admission, got {other:?}"),
    };
    assert_eq!(segments.len(), 1);
    assert_eq!(released.len(), 2, "must free until the 75% target is met");
    assert_eq!(released[0].entries[0].entry_id, "s1:c");
    assert_eq!(released[1].entries[0].entry_id, "s1:b");
    assert!(released.iter().all(|plan| plan.segment != a));
    assert!(window.segment(a).is_some(), "viewport segment is pinned");
    assert!(window.segment(b).is_none());
    assert!(window.segment(c).is_none());
    assert_eq!(window.segment_count(), 2);
    assert_eq!(window.gap_count(), 1);
    assert_eq!(window.budget().resident_body_bytes, 600);
    assert_eq!(window.take_pending_release_plans().len(), 2);
    window.settle_pending_merge();
}

/// 工具卡归属后成为可淘汰 LiveSeal 段：同一批字节从「只增不减的 unattributed」
/// 转入可释放段，预算压力能生成释放计划并接纳新页。
#[test]
fn tool_live_seal_segment_is_evictable_while_unattributed_bytes_are_not() {
    let mut window = enabled_window(small_limits(1_000));
    window.note_unattributed_live_bytes(1_000);
    let rejected = admit(
        &mut window,
        "r1",
        segment("page-a", 10, 200, 220),
        PagePlacement::Tail,
    );
    assert!(
        matches!(
            rejected,
            AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::EvictionBlocked(_),
                ..
            })
        ),
        "unattributed live bytes cannot free capacity"
    );

    // 归属成功：字节移出 unattributed，登记为带持久位置的 LiveSeal 工具段。
    window.release_unattributed_live_bytes(1_000);
    window
        .register_segment(
            segment("tool-entry", 5, 900, 940).origin(SegmentOrigin::LiveSeal),
            SegmentPlacement::Tail,
        )
        .expect("live tool segment");

    let admitted = admit(
        &mut window,
        "r2",
        segment("page-b", 20, 200, 220),
        PagePlacement::Tail,
    );
    let released = match admitted {
        AdmissionDecision::Accepted { released, .. } => released,
        other => panic!("attributed tool bytes must be evictable, got {other:?}"),
    };
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].entries[0].entry_id, "s1:tool-entry");
    assert_eq!(window.budget().unattributed_live_bytes, 0);
}

#[test]
fn all_pinned_segments_reject_admission_and_report_reasons() {
    let mut window = enabled_window(small_limits(1_000));
    let a = accepted_key(admit_and_settle(
        &mut window,
        "r1",
        segment("a", 10, 400, 500),
    ));
    let b = accepted_key(admit_and_settle(
        &mut window,
        "r2",
        segment("b", 20, 600, 700),
    ));
    assert_eq!(window.budget().resident_body_bytes, 1_000);

    let anchor_entry = window.segment(a).unwrap().entries[0].key.clone();
    window.set_anchor(Some(WindowAnchor::new(
        anchor_entry,
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    window.pin(b, PinReason::Running);

    let decision = admit(
        &mut window,
        "r3",
        segment("c", 30, 100, 120),
        PagePlacement::Tail,
    );
    let rejection = match decision {
        AdmissionDecision::Rejected(rejection) => rejection,
        other => panic!("expected rejection, got {other:?}"),
    };
    let AdmissionRejectionReason::EvictionBlocked(blocked) = rejection.reason else {
        panic!("expected eviction blocked, got {:?}", rejection.reason);
    };
    assert_eq!(blocked.pinned.pinned_segments, 2);
    assert_eq!(blocked.pinned.pinned_bytes, 1_000);
    assert!(blocked.pinned.reasons.contains(&PinReason::Viewport));
    assert!(blocked.pinned.reasons.contains(&PinReason::Running));
    assert!(rejection.reservation_released);
    assert_eq!(window.budget().page_reserved_bytes, 0);
    assert_eq!(window.segment_count(), 2);
    assert_eq!(window.stats().evict_blocked_pinned, 1);
    assert_eq!(window.stats().admission_blocked, 1);
    assert_eq!(window.diagnostics().evict_blocked_pinned, 1);
}

#[test]
fn stale_page_generation_is_rejected_and_reservation_is_released() {
    let mut window = enabled_window(small_limits(10_000));
    assert!(matches!(
        window.reserve_page("late", 200),
        ReserveOutcome::Granted(_)
    ));
    let decision = window.admit_page(PageDraft {
        request_id: "late".into(),
        window_generation: window.generation() + 1,
        identity: identity(),
        placement: PagePlacement::Tail,
        segments: vec![segment("a", 10, 200, 300)],
    });
    assert!(matches!(
        decision,
        AdmissionDecision::Rejected(AdmissionRejection {
            reason: AdmissionRejectionReason::StaleIdentity { .. },
            reservation_released: true,
            ..
        })
    ));
    assert_eq!(window.budget().page_reserved_bytes, 0);
    assert_eq!(window.segment_count(), 0);
    assert_eq!(window.stats().stale_pages, 1);
}

#[test]
fn one_pending_merge_page_blocks_the_next_admission() {
    let mut window = enabled_window(small_limits(10_000));
    assert!(matches!(
        admit(
            &mut window,
            "r1",
            segment("a", 10, 100, 120),
            PagePlacement::Tail
        ),
        AdmissionDecision::Accepted { .. }
    ));
    assert_eq!(window.pending_merge_bytes(), 100);

    let decision = admit(
        &mut window,
        "r2",
        segment("b", 20, 100, 120),
        PagePlacement::Tail,
    );
    assert!(matches!(
        decision,
        AdmissionDecision::Rejected(AdmissionRejection {
            reason: AdmissionRejectionReason::PendingMergeLimit,
            reservation_released: true,
            ..
        })
    ));
    assert_eq!(window.segment_count(), 1);
    assert_eq!(window.budget().page_reserved_bytes, 0);
    window.settle_pending_merge();
}

#[test]
fn low_level_release_plan_cannot_bypass_pins_or_non_durable_segments() {
    let mut window = enabled_window(small_limits(10_000));
    let a = accepted_key(admit_and_settle(
        &mut window,
        "r1",
        segment("a", 10, 100, 120),
    ));
    let b = accepted_key(admit_and_settle(
        &mut window,
        "r2",
        segment("b", 20, 100, 120),
    ));
    window.pin(a, PinReason::Running);

    let pinned_plan = release_plan_for(&window, a);
    assert!(window.apply_release_plan(&pinned_plan).is_none());
    assert!(window.segment(a).is_some());

    let unpinned_plan = release_plan_for(&window, b);
    assert!(window.apply_release_plan(&unpinned_plan).is_some());
    assert!(window.segment(b).is_none());
}

#[test]
fn degraded_window_stops_eviction_and_rejects_new_pages_but_keeps_segments() {
    let mut window = enabled_window(small_limits(1_000));
    let a = accepted_key(admit_and_settle(
        &mut window,
        "r1",
        segment("a", 10, 400, 500),
    ));
    window.set_degraded("cursor expired");
    assert!(window.is_degraded());
    assert!(matches!(
        window.reserve_page("r2", 100),
        ReserveOutcome::Rejected(ReserveRejection::Degraded { .. })
    ));
    let decision = window.admit_page(PageDraft {
        request_id: "r2".into(),
        window_generation: window.generation(),
        identity: identity(),
        placement: PagePlacement::Tail,
        segments: vec![segment("b", 20, 100, 120)],
    });
    assert!(matches!(
        decision,
        AdmissionDecision::Rejected(AdmissionRejection {
            reason: AdmissionRejectionReason::Degraded { .. },
            ..
        })
    ));
    assert!(window.plan_eviction(500).plans.is_empty());
    assert!(window.segment(a).is_some());
    assert!(window.diagnostics().window_degraded);
    window.mark_ready();
    assert!(!window.is_degraded());
}

#[test]
fn reservation_limits_are_independent_from_body_budget() {
    let mut window = enabled_window(WindowLimits {
        body_bytes: 10_000,
        reserved_bytes: 1_000,
        max_in_flight_pages: 1,
        ..WindowLimits::default()
    });
    assert!(matches!(
        window.reserve_page("r1", 600),
        ReserveOutcome::Granted(_)
    ));
    assert!(matches!(
        window.reserve_page("r2", 100),
        ReserveOutcome::Rejected(ReserveRejection::TooManyInFlight { .. })
    ));
    assert!(matches!(
        window.reserve_page("r1", 600),
        ReserveOutcome::Rejected(ReserveRejection::DuplicateRequest)
    ));
    window.release_page_reservation("r1");
    assert!(matches!(
        window.reserve_page("r3", 1_001),
        ReserveOutcome::Rejected(ReserveRejection::ReservedBudgetExceeded { .. })
    ));
    assert_eq!(window.budget().resident_body_bytes, 0);
}
