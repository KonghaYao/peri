use std::collections::{HashMap, VecDeque};

use super::test_support::*;
use super::*;

#[test]
fn adjacent_gaps_merge_outer_boundaries_and_inner_cursors() {
    let a = PersistentEntryKey::own("s1:a");
    let b = PersistentEntryKey::own("s1:b");
    let c = PersistentEntryKey::own("s1:c");
    let mut layout = VecDeque::from([RegionKey::Gap(GapId(1)), RegionKey::Gap(GapId(2))]);
    let mut gaps = HashMap::from([
        (
            GapId(1),
            HistoryGap::new(
                GapId(1),
                Some(a.clone()),
                Some(b.clone()),
                Some("before-1".into()),
                Some("after-1".into()),
                HistorySource::Own,
                GapPlaceholder::OlderHistory,
            ),
        ),
        (
            GapId(2),
            HistoryGap::new(
                GapId(2),
                Some(b),
                Some(c.clone()),
                Some("before-2".into()),
                Some("after-2".into()),
                HistorySource::Own,
                GapPlaceholder::MissingMiddle,
            ),
        ),
    ]);

    merge_adjacent_gaps(&mut layout, &mut gaps);
    assert_eq!(layout, VecDeque::from([RegionKey::Gap(GapId(1))]));
    let merged = gaps.get(&GapId(1)).expect("merged gap");
    assert_eq!(merged.before, Some(a));
    assert_eq!(merged.after, Some(c));
    // 游标取合并后缺口的内侧边界：上端来自更晚 gap 的 before 游标，
    // 下端来自更早 gap 的 after 游标；外侧游标锚在已被淘汰的段上，无法回填缺口。
    assert_eq!(merged.before_cursor.as_deref(), Some("before-2"));
    assert_eq!(merged.after_cursor.as_deref(), Some("after-1"));
    assert_eq!(merged.placeholder, GapPlaceholder::MissingMiddle);
    assert_eq!(gaps.len(), 1);
}

#[test]
fn anchor_on_resident_entry_keeps_identity_and_pins_viewport_segment() {
    let mut window = enabled_window(WindowLimits::default());
    let key = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("segment");
    let entry = window.segment(key).unwrap().entries[0].key.clone();
    let relocation = window.set_anchor(Some(WindowAnchor::new(
        entry.clone(),
        Some(0),
        WindowAnchorOffset::BlockOffset(3),
    )));
    assert_eq!(
        relocation,
        AnchorRelocation::Resident {
            segment: key,
            entry,
            sub_item: Some(0),
        }
    );
    assert!(
        window
            .segment(key)
            .unwrap()
            .pins
            .contains(PinReason::Viewport)
    );
    assert_eq!(window.budget().pinned_body_bytes, 10);
}

#[test]
fn missing_anchor_relocates_to_gap_and_selects_seek_target() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("a");
    let mut draft = segment("b", 20, 10, 20);
    draft.before_cursor = Some("before-b".into());
    draft.after_cursor = Some("after-b".into());
    let b = window
        .register_segment(draft, SegmentPlacement::Tail)
        .expect("b");
    let c = window
        .register_segment(segment("c", 30, 10, 20), SegmentPlacement::Tail)
        .expect("c");

    let b_entry = window.segment(b).unwrap().entries[0].key.clone();
    let plan = release_plan_for(&window, b);
    window.apply_release_plan(&plan).expect("release b");
    assert_eq!(window.gap_count(), 1);
    let (gap_id, gap_before, gap_after) = {
        let gap = window.gaps().values().next().expect("gap");
        (gap.id, gap.before.clone(), gap.after.clone())
    };
    assert_eq!(
        gap_before.as_ref(),
        window
            .segment(a)
            .unwrap()
            .entries
            .first()
            .map(|entry| &entry.key)
    );
    assert_eq!(
        gap_after.as_ref(),
        window
            .segment(c)
            .unwrap()
            .entries
            .last()
            .map(|entry| &entry.key)
    );

    let relocation = window.set_anchor(Some(WindowAnchor::new(
        b_entry.clone(),
        None,
        WindowAnchorOffset::CharOffset(12),
    )));
    match relocation {
        AnchorRelocation::Gap {
            gap_id: relocated_gap,
            seek: Some(GapSeek::Seek { anchor, source, .. }),
        } => {
            assert_eq!(relocated_gap, gap_id);
            assert_eq!(anchor, b_entry);
            assert_eq!(source, HistorySource::Own);
        }
        other => panic!("expected gap seek relocation, got {other:?}"),
    }
    assert_eq!(window.budget().pinned_body_bytes, 0);
}

#[test]
fn two_released_segments_coalesce_into_one_gap() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("a");
    let b = window
        .register_segment(segment("b", 20, 10, 20), SegmentPlacement::Tail)
        .expect("b");
    let c = window
        .register_segment(segment("c", 30, 10, 20), SegmentPlacement::Tail)
        .expect("c");
    let d = window
        .register_segment(segment("d", 40, 10, 20), SegmentPlacement::Tail)
        .expect("d");

    let plan_b = release_plan_for(&window, b);
    window.apply_release_plan(&plan_b).expect("release b");
    let plan_c = release_plan_for(&window, c);
    window.apply_release_plan(&plan_c).expect("release c");

    assert_eq!(window.gap_count(), 1);
    let gap = window.gaps().values().next().expect("gap");
    assert_eq!(
        gap.before.as_ref(),
        window
            .segment(a)
            .unwrap()
            .entries
            .first()
            .map(|entry| &entry.key)
    );
    assert_eq!(
        gap.after.as_ref(),
        window
            .segment(d)
            .unwrap()
            .entries
            .first()
            .map(|entry| &entry.key)
    );
    assert_eq!(
        window
            .layout()
            .iter()
            .filter(|region| matches!(region, RegionKey::Gap(_)))
            .count(),
        1
    );
}

#[test]
fn coalesced_gap_keeps_cursors_to_resident_edges() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(
            segment("a", 10, 10, 20).cursors(Some("before-a".into()), Some("after-a".into())),
            SegmentPlacement::Tail,
        )
        .expect("a");
    let b = window
        .register_segment(
            segment("b", 20, 10, 20).cursors(Some("before-b".into()), Some("after-b".into())),
            SegmentPlacement::Tail,
        )
        .expect("b");
    let c = window
        .register_segment(
            segment("c", 30, 10, 20).cursors(Some("before-c".into()), Some("after-c".into())),
            SegmentPlacement::Tail,
        )
        .expect("c");
    let d = window
        .register_segment(
            segment("d", 40, 10, 20).cursors(Some("before-d".into()), Some("after-d".into())),
            SegmentPlacement::Tail,
        )
        .expect("d");

    let plan_b = release_plan_for(&window, b);
    window.apply_release_plan(&plan_b).expect("release b");
    let plan_c = release_plan_for(&window, c);
    window.apply_release_plan(&plan_c).expect("release c");

    assert_eq!(window.gap_count(), 1);
    let gap = window.gaps().values().next().expect("gap");
    assert_eq!(
        gap.before_cursor.as_deref(),
        Some("before-d"),
        "缺口上端必须锚在更晚的驻留边缘 D，而不是已淘汰的 B/C"
    );
    assert_eq!(
        gap.after_cursor.as_deref(),
        Some("after-a"),
        "缺口下端必须锚在更早的驻留边缘 A，而不是已淘汰的 B/C"
    );
    let _ = (a, d);
}

#[test]
fn open_ended_gap_prefers_page_cursor_over_seek() {
    let gap = HistoryGap::new(
        GapId(7),
        None,
        Some(PersistentEntryKey::own("s1:newest")),
        Some("older-cursor".into()),
        None,
        HistorySource::Own,
        GapPlaceholder::OlderHistory,
    );
    let anchor = WindowAnchor::new(
        PersistentEntryKey::own("s1:missing"),
        None,
        WindowAnchorOffset::CharOffset(0),
    );
    assert_eq!(
        select_gap_seek(&gap, &anchor),
        Some(GapSeek::Page {
            gap_id: GapId(7),
            direction: GapSeekDirection::Before,
            cursor: "older-cursor".into(),
        })
    );
}

#[test]
fn gap_recovery_maps_missing_anchor_to_seek_or_page_request() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("a");
    let b = window
        .register_segment(segment("b", 20, 10, 20), SegmentPlacement::Tail)
        .expect("b");
    let c = window
        .register_segment(segment("c", 30, 10, 20), SegmentPlacement::Tail)
        .expect("c");
    let b_entry = window.segment(b).unwrap().entries[0].key.clone();
    let plan = release_plan_for(&window, b);
    window.apply_release_plan(&plan).expect("release b");
    window.set_anchor(Some(WindowAnchor::new(
        b_entry.clone(),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    assert!(matches!(
        crate::kit::history_paging::window_gap_recovery(&window),
        Some(crate::kit::history_paging::WindowGapRequest::Seek { anchor, .. })
            if anchor == b_entry
    ));
    let _ = (a, c);
}

#[test]
fn gap_uses_inner_neighbor_cursors() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(
            segment("a", 10, 10, 20).cursors(Some("before-a".into()), Some("after-a".into())),
            SegmentPlacement::Tail,
        )
        .expect("a");
    let b = window
        .register_segment(
            segment("b", 20, 10, 20).cursors(Some("before-b".into()), Some("after-b".into())),
            SegmentPlacement::Tail,
        )
        .expect("b");
    let c = window
        .register_segment(
            segment("c", 30, 10, 20).cursors(Some("before-c".into()), Some("after-c".into())),
            SegmentPlacement::Tail,
        )
        .expect("c");

    let plan = release_plan_for(&window, b);
    window.apply_release_plan(&plan).expect("release b");

    let gap = window.gaps().values().next().expect("gap");
    assert_eq!(
        gap.before_cursor.as_deref(),
        Some("before-c"),
        "page(Before) must refill from the later resident edge"
    );
    assert_eq!(
        gap.after_cursor.as_deref(),
        Some("after-a"),
        "page(After) must refill from the earlier resident edge"
    );
    let _ = (a, c);
}

#[test]
fn anchor_relocation_uses_identity_not_layout_index() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("a");
    let b = window
        .register_segment(segment("b", 20, 10, 20), SegmentPlacement::Tail)
        .expect("b");
    let b_entry = window.segment(b).unwrap().entries[0].key.clone();
    window.set_anchor(Some(WindowAnchor::new(
        b_entry.clone(),
        None,
        WindowAnchorOffset::BlockOffset(4),
    )));
    // 在锚点之前插入一个新段：布局下标变化，但身份重定位仍指向 B。
    window
        .register_segment(segment("c", 5, 10, 20), SegmentPlacement::BeforeSegment(a))
        .expect("c");
    assert_eq!(
        window.relocate_anchor(),
        AnchorRelocation::Resident {
            segment: b,
            entry: b_entry,
            sub_item: None,
        }
    );
}

#[test]
#[serial_test::serial]
fn gap_request_atom_holds_seek_target_until_content_returns() {
    crate::kit::atoms::init_atoms();
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("a");
    let b = window
        .register_segment(segment("b", 20, 10, 20), SegmentPlacement::Tail)
        .expect("b");
    let c = window
        .register_segment(segment("c", 30, 10, 20), SegmentPlacement::Tail)
        .expect("c");
    let b_entry = window.segment(b).unwrap().entries[0].key.clone();
    let plan = release_plan_for(&window, b);
    window.apply_release_plan(&plan).expect("release b");
    window.set_anchor(Some(WindowAnchor::new(
        b_entry.clone(),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    crate::kit::history_paging::publish_window_gap_request(&window);
    let handle = crate::kit::atoms::WINDOW_GAP_REQUEST.state();
    let request = handle
        .read()
        .iter()
        .find(|request| {
            matches!(
                request,
                crate::kit::history_paging::WindowGapRequest::Seek { .. }
            )
        })
        .cloned()
        .expect("gap request");
    match request {
        crate::kit::history_paging::WindowGapRequest::Seek { anchor, .. } => {
            assert_eq!(anchor, b_entry);
        }
        other => panic!("expected seek gap request, got {other:?}"),
    }
    crate::kit::history_window::clear_window_viewport();
    assert!(
        crate::kit::atoms::WINDOW_GAP_REQUEST
            .state()
            .read()
            .is_empty()
    );
    let _ = (a, c);
}

#[test]
#[serial_test::serial]
fn viewport_atom_publishes_window_generation_and_stable_anchor() {
    crate::kit::atoms::init_atoms();
    let mut window = enabled_window(WindowLimits::default());
    let key = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("a");
    let entry = window.segment(key).unwrap().entries[0].key.clone();
    window.set_anchor(Some(WindowAnchor::new(
        entry.clone(),
        None,
        WindowAnchorOffset::CharOffset(3),
    )));
    publish_window_viewport(&window);
    let viewport = crate::kit::atoms::WINDOW_VIEWPORT
        .state()
        .read()
        .clone()
        .expect("published viewport");
    assert_eq!(viewport.window_generation, window.generation());
    assert_eq!(viewport.anchor.as_ref().unwrap().entry, entry);
    assert_eq!(
        viewport.relocation,
        Some(AnchorRelocation::Resident {
            segment: key,
            entry,
            sub_item: None,
        })
    );
    clear_window_viewport();
    assert!(crate::kit::atoms::WINDOW_VIEWPORT.state().read().is_none());
}
