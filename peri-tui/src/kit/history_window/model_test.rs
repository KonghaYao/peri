use super::AdmissionDecision;
use super::HistoryEntryView;
use super::HistorySource;
use super::HistoryWindow;
use super::PageDraft;
use super::PagePlacement;
use super::PersistentEntryKey;
use super::PersistentSubItemKey;
use super::PinReason;
use super::SegmentDraft;
use super::SegmentPlacement;
use super::WindowLimits;

#[test]
fn display_overlap_respects_adjacent_ranges_and_gaps() {
    let mut window = super::test_support::enabled_window(WindowLimits::default());
    assert!(window.mark_existing_display_slot(PersistentEntryKey::own("a"), 2, 2));
    assert!(window.mark_existing_display_slot(PersistentEntryKey::own("b"), 6, 2));
    for slot in [0, 1, 4, 5, 8, 9] {
        assert!(window.display_slot_available(slot), "空隙不属于历史条目");
    }
    for slot in [2, 3, 6, 7] {
        assert!(
            !window.display_slot_available(slot),
            "持久历史单元不能按提示淘汰"
        );
    }
    assert!(window.display.overlaps(3, 4));
    assert!(!window.display.overlaps(4, 2));
}

#[test]
fn releasing_disjoint_entries_shifts_each_survivor_and_preserves_unmapped_gaps() {
    let key = PersistentEntryKey::own;
    let mut ledger = super::display::DisplayLedger::default();
    for (id, slot, len) in [
        ("a", 1, 2),
        ("b", 4, 3),
        ("c", 8, 2),
        ("d", 10, 1),
        ("e", 12, 2),
    ] {
        assert!(ledger.insert_existing_slot(key(id), slot, len));
    }
    ledger.insert(key("external"), Some(52));
    ledger.insert_block(&[(key("before"), 2)], 4);
    assert_eq!(
        ledger.remove_entries_at(&[key("b"), key("d"), key("external")].into()),
        [(6, 3), (12, 1)]
    );
    assert_eq!(
        ledger.debug_ranges(),
        [
            (1, key("a"), 2),
            (4, key("before"), 2),
            (7, key("c"), 2),
            (10, key("e"), 2)
        ]
    );
    assert!(!ledger.contains(&key("external")));
    assert!(ledger.remove_entries_at(&[key("b")].into()).is_empty());
    assert_eq!(ledger.len(), 4);
}

#[test]
fn page_placement_finds_a_resident_display_range_after_a_live_slot() {
    let mut window = super::test_support::enabled_window(WindowLimits::default());
    for (id, position) in [("a", 1), ("b", 2)] {
        window
            .register_segment(
                super::test_support::segment(id, position, 10, 10),
                SegmentPlacement::Tail,
            )
            .unwrap();
    }
    let a = PersistentEntryKey::own("s1:a");
    let b = PersistentEntryKey::own("s1:b");
    assert_eq!(window.index().entry_keys_in_slot_order().next(), Some(&a));
    assert!(window.mark_existing_display_slot(a.clone(), 2, 1));
    assert!(window.mark_existing_display_slot(PersistentEntryKey::own("s1:live"), 4, 1));
    // The same live entry may own several disjoint display ranges.
    assert!(window.mark_existing_display_slot(a.clone(), 5, 1));
    assert!(window.mark_existing_display_slot(b.clone(), 7, 1));
    assert_eq!(window.resident_entry_at_or_after_slot(2), Some(&a));
    assert_eq!(window.resident_entry_at_or_after_slot(3), Some(&b));
    assert_eq!(window.resident_entry_at_or_after_slot(8), None);
}

#[test]
fn disposable_preview_removal_preserves_canonical_display_ranges() {
    let mut window = super::test_support::enabled_window(WindowLimits::default());
    let first = PersistentEntryKey::own("s1:first");
    let later = PersistentEntryKey::own("s1:later");
    assert!(window.mark_existing_display_slot(first.clone(), 0, 2));
    assert!(window.mark_existing_display_slot(later.clone(), 3, 2));
    // Slot 2 is a disposable child preview between two canonical entries.
    assert!(window.remove_unmapped_display_slot(2));
    assert_eq!(window.display.range_for(&first), Some((0, 2)));
    assert_eq!(window.display.range_for(&later), Some((2, 2)));
    assert!(!window.remove_unmapped_display_slot(3));
    assert_eq!(window.display.range_for(&later), Some((2, 2)));
    assert!(window.is_entry_displayed(&first));
    assert!(window.is_entry_displayed(&later));
}

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
    assert_eq!(limits.detail_bytes, 4 * 1024 * 1024);
    assert_eq!(limits.release_watermark_percent, 75);
    assert_eq!(limits.prefetch_threshold_percent, 50);
    assert_eq!(limits.max_in_flight_pages, 1);
    assert_eq!(limits.max_pending_merge_pages, 1);
}

#[test]
fn window_budget_separates_body_reserved_and_pinned() {
    let mut window = super::test_support::enabled_window(WindowLimits {
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
    let window = super::test_support::enabled_window(WindowLimits {
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
fn prefetch_counts_body_still_held_by_the_renderer() {
    let mut window = super::test_support::enabled_window(WindowLimits {
        body_bytes: 1_000,
        ..WindowLimits::default()
    });
    window
        .register_segment(segment("a", 1, 400, 400), SegmentPlacement::Tail)
        .expect("segment");
    assert!(window.diagnostics().can_prefetch);
    window.record_pending_release(100, 3);
    assert!(!window.diagnostics().can_prefetch);
    window.ack_release_epoch(2);
    assert!(!window.diagnostics().can_prefetch);
    window.ack_release_epoch(3);
    assert!(window.diagnostics().can_prefetch);
}

#[test]
fn inherited_segment_is_pinned_and_never_evictable() {
    let mut window = super::test_support::enabled_window(WindowLimits::default());
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
    let mut window = super::test_support::enabled_window(WindowLimits {
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
        identity: super::test_support::identity(),
        placement: PagePlacement::Tail,
        segments: vec![segment("a", 1, 10, 20)],
    });
    assert_eq!(decision, AdmissionDecision::Disabled);
    assert_eq!(window.segment_count(), 0);
}

#[test]
#[serial_test::serial]
fn history_window_beta_defaults_true_and_only_explicit_false_disables() {
    crate::kit::atoms::init_atoms();
    let mut config = crate::config::PeriConfig::default();

    // 未配置 = 开启（S7 发布启用）：快照投影与回退配置读取必须一致。
    assert!(super::history_window_beta_enabled(&config));
    assert!(super::history_window_beta_enabled_from_flags(
        &peri_config::betas::from_merged(&config)
    ));
    super::apply_history_window_beta(super::history_window_beta_enabled(&config));
    assert!(crate::kit::atoms::HISTORY_WINDOW_V1.get());

    // 显式 false 仍关闭。
    config
        .config
        .betas
        .set(peri_config::betas::HISTORY_WINDOW_V1, false);
    assert!(!super::history_window_beta_enabled(&config));
    assert!(!super::history_window_beta_enabled_from_flags(
        &peri_config::betas::from_merged(&config)
    ));
    super::apply_history_window_beta(false);
    assert!(!crate::kit::atoms::HISTORY_WINDOW_V1.get());

    // 显式 true 开启。
    config
        .config
        .betas
        .set(peri_config::betas::HISTORY_WINDOW_V1, true);
    assert!(super::history_window_beta_enabled(&config));
    assert!(super::history_window_beta_enabled_from_flags(
        &peri_config::betas::from_merged(&config)
    ));
    super::apply_history_window_beta(true);
    assert!(crate::kit::atoms::HISTORY_WINDOW_V1.get());
    assert!(
        !(crate::kit::atoms::HISTORY_WINDOW_V1.get()
            && crate::kit::atoms::HISTORY_WINDOW_CAPABLE.get()),
        "local projection alone must not force the window path without server capability"
    );
    super::apply_history_window_beta(false);
}

use super::test_support::segment;
