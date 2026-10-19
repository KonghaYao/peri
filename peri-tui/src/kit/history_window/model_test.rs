use super::test_support::*;
use super::*;

#[test]
fn history_window_identity_separates_source_entry_and_sub_item() {
    let own = PersistentEntryKey::own("s1:message-1");
    let inherited = PersistentEntryKey::new(HistorySource::inherited("snap-1"), "s1:message-1");
    assert_ne!(
        own, inherited,
        "same entryId from different sources is not one identity"
    );

    let own_sub = PersistentSubItemKey::new(own.clone(), 0);
    let inherited_sub = PersistentSubItemKey::new(inherited, 0);
    assert_ne!(own_sub, inherited_sub);
    assert_eq!(own_sub.entry, own);

    let own_sub_1 = PersistentSubItemKey::new(own.clone(), 1);
    assert_ne!(own_sub, own_sub_1, "sub item index is part of identity");
}

#[test]
fn default_window_budget_matches_plan_initial_values() {
    let limits = WindowLimits::default();
    assert_eq!(limits.body_bytes, 16 * 1024 * 1024);
    assert_eq!(limits.reserved_bytes, 2 * 1024 * 1024);
    assert_eq!(limits.page_target_bytes, 512 * 1024);
    assert_eq!(limits.pending_tool_bytes, 1024 * 1024);
    assert_eq!(limits.detail_bytes, 4 * 1024 * 1024);
    assert_eq!(limits.release_watermark_percent, 75);
    assert_eq!(limits.prefetch_threshold_percent, 50);
    assert_eq!(limits.max_in_flight_pages, 1);
    assert_eq!(limits.max_pending_merge_pages, 1);
    assert_eq!(limits.max_pending_tool_results, 256);
}

#[test]
fn window_budget_separates_body_reserved_and_pinned() {
    let mut window = enabled_window(WindowLimits {
        body_bytes: 1_000,
        ..WindowLimits::default()
    });
    let key = window
        .register_segment(segment("a", 10, 400, 600), SegmentPlacement::Tail)
        .expect("segment");
    assert_eq!(window.budget().resident_body_bytes, 400);
    assert_eq!(window.budget().resident_reserved_bytes, 600);
    assert_eq!(window.budget().pinned_body_bytes, 0);

    window.pin(key, PinReason::Viewport);
    assert_eq!(window.budget().pinned_body_bytes, 400);
    assert_eq!(window.budget().evictable_body_bytes(), 0);
}

#[test]
fn release_target_uses_watermark_and_remaining_capacity() {
    let window = enabled_window(WindowLimits {
        body_bytes: 1_000,
        release_watermark_percent: 75,
        ..WindowLimits::default()
    });
    assert_eq!(window.budget().watermark_bytes(), 750);
    assert_eq!(window.budget().prefetch_threshold_bytes(), 500);
    assert_eq!(window.budget().release_target_bytes(100), 750);
    assert_eq!(window.budget().release_target_bytes(400), 600);
    assert_eq!(window.budget().release_target_bytes(1_500), 0);
}

#[test]
fn inherited_segment_is_pinned_and_never_evictable() {
    let mut window = enabled_window(WindowLimits::default());
    let source = HistorySource::inherited("snap-42");
    let entry = HistoryEntryView::new(
        PersistentEntryKey::new(source.clone(), "ancestor:1"),
        None,
        1,
        120,
        160,
        Vec::new(),
    );
    let key = window
        .register_segment(
            SegmentDraft::new(source, vec![entry], 120, 160),
            SegmentPlacement::Tail,
        )
        .expect("inherited segment");
    let segment = window.segment(key).expect("segment");
    assert!(segment.pins.contains(PinReason::InheritedUnsupported));
    assert_eq!(window.budget().pinned_body_bytes, 120);
}

#[test]
fn checkpoint_history_is_bounded_and_deduplicated() {
    let mut window = enabled_window(WindowLimits {
        checkpoint_history: 2,
        ..WindowLimits::default()
    });
    assert!(window.note_checkpoint("cp-1"));
    assert!(!window.note_checkpoint("cp-1"));
    assert!(window.note_checkpoint("cp-2"));
    assert!(window.note_checkpoint("cp-3"));
    assert!(!window.is_checkpoint_seen("cp-1"));
    assert!(window.is_checkpoint_seen("cp-3"));
}

#[test]
fn disabled_window_does_not_admit_pages() {
    let mut window = HistoryWindow::default();
    let decision = window.admit_page(PageDraft {
        request_id: "r1".into(),
        window_generation: window.generation(),
        identity: identity(),
        placement: PagePlacement::Tail,
        segments: vec![segment("a", 1, 10, 20)],
    });
    assert_eq!(decision, AdmissionDecision::Disabled);
    assert_eq!(window.segment_count(), 0);
}

#[test]
#[serial_test::serial]
fn history_window_beta_defaults_false_and_writes_atom_once() {
    crate::kit::atoms::init_atoms();
    let mut config = crate::config::PeriConfig::default();
    assert!(!history_window_beta_enabled(&config));
    apply_history_window_beta(false);
    assert!(!crate::kit::atoms::HISTORY_WINDOW_V1.get());

    config
        .config
        .betas
        .set(peri_config::betas::HISTORY_WINDOW_V1, true);
    assert!(history_window_beta_enabled(&config));
    apply_history_window_beta(true);
    assert!(crate::kit::atoms::HISTORY_WINDOW_V1.get());
    assert!(
        !(crate::kit::atoms::HISTORY_WINDOW_V1.get()
            && crate::kit::atoms::HISTORY_WINDOW_CAPABLE.get()),
        "config alone must not force the window path without server capability"
    );
    apply_history_window_beta(false);
}
