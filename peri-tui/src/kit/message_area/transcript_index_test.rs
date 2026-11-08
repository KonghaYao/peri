use super::*;

#[test]
#[serial_test::serial]
fn visible_tool_header_hit_skips_process_step_and_body_at_each_breakpoint() {
    use crate::kit::entry_render_cache::EntrySurface;
    use crate::kit::tui_render_unit::{FoldState, TuiToolCard, TuiToolPresentation, TuiUserBubble};
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    let context = EntryRenderContext {
        theme: Arc::clone(&peri_theme::atoms::THEME_ATOM.state().read()),
        language: crate::kit::atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    };
    let items = im::vector![
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("read the file".into())),
        TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_id: "clicked-tool".into(),
            tool_name: "Read".into(),
            input_summary: "fixture.txt".into(),
            output_summary: "tool-body".into(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            completed_duration_ms: None,
            diff: None,
            presentation: TuiToolPresentation::Generic,
            fold: FoldState::Expanded,
            user_modified: false,
            tool_calls_count: 0,
            content_hash: 0,
        }),
    ];
    for width in [100, 64, 50] {
        let grid = GridSpec::grid_for(width);
        let mut index = SlotIndex::default();
        index.set_source(items.clone(), grid, context.clone(), 0);
        for (slot, item) in items.iter().enumerate() {
            let mut cache = EntryRenderCache::default();
            cache.ensure(
                item,
                &grid,
                context.clone(),
                0,
                super::super::transcript::zone_transition(&items, slot),
            );
            let (_, map) =
                super::super::selection::build_wrap_map(cache.lines(), grid.line_width());
            index.set_slot(
                slot,
                Arc::new(SlotLines::single(Arc::clone(cache.lines()))),
                &map,
                true,
            );
        }
        let start = index.slot_visual_start(1).unwrap();
        let step_rows = usize::from(grid.indent > 0);
        assert!(
            index
                .line(1, step_rows)
                .unwrap()
                .to_string()
                .contains("Read fixture.txt")
        );
        assert!(
            index
                .line(1, step_rows + 1)
                .unwrap()
                .to_string()
                .contains("tool-body")
        );
        let hit =
            |row| super::super::scroll::freeze_down_index((8, 8), (row, 8), &index, true).entry_hit;
        assert_eq!(
            hit(start + step_rows),
            Some((1, 0)),
            "visible header at width {width}"
        );
        if step_rows > 0 {
            assert_eq!(hit(start), None, "decorative step is not a header");
        }
        assert_eq!(
            hit(start + step_rows + 1),
            None,
            "body text keeps selection semantics"
        );
    }
}

fn slot(text: &str, height: usize) -> (Arc<SlotLines>, Vec<WrappedLineInfo>) {
    (
        Arc::new(SlotLines::single(Arc::new(vec![Line::from(
            text.to_owned(),
        )]))),
        vec![WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: height,
            slot_index: 0,
        }],
    )
}

#[test]
fn persistent_update_keeps_old_layout_and_untouched_branch() {
    let mut index = SlotIndex::default();
    for position in 0..1024 {
        let (lines, map) = slot(&format!("row-{position}"), 1);
        index.set_slot(position, lines, &map, true);
    }
    let old = index.clone();
    let (lines, map) = slot("changed 中文🙂", 7);
    index.set_slot(1023, lines, &map, true);
    let NodeKind::Branch(old_left, _) = &old.root.as_ref().unwrap().kind else {
        panic!()
    };
    let NodeKind::Branch(new_left, _) = &index.root.as_ref().unwrap().kind else {
        panic!()
    };
    assert!(Arc::ptr_eq(
        old_left.as_ref().unwrap(),
        new_left.as_ref().unwrap()
    ));
    assert_eq!(old.total_visual(), 1024);
    assert_eq!(index.total_visual(), 1030);
    assert_eq!(old.line(1023, 0).unwrap().to_string(), "row-1023");
    assert_eq!(index.visual_lookup(1029).unwrap().slot_index, 1023);
    assert_eq!(index.slot_visual_start(1023), Some(1023));
    assert_eq!(changed_nodes(&old.root, &index.root), 11);
}

fn changed_nodes(old: &Option<Arc<Node>>, new: &Option<Arc<Node>>) -> usize {
    match (old, new) {
        (Some(old), Some(new)) if Arc::ptr_eq(old, new) => 0,
        (Some(old), Some(new)) => match (&old.kind, &new.kind) {
            (NodeKind::Branch(old_left, old_right), NodeKind::Branch(new_left, new_right)) => {
                1 + changed_nodes(old_left, new_left) + changed_nodes(old_right, new_right)
            }
            _ => 1,
        },
        (None, None) => 0,
        _ => 1,
    }
}

#[test]
fn old_layout_does_not_keep_evictable_lines_alive() {
    let mut index = SlotIndex::default();
    let (lines, map) = slot("heavy allocation", 3);
    let weak = Arc::downgrade(&lines);
    index.set_slot(0, Arc::clone(&lines), &map, false);
    let old = index.clone();
    drop(lines);
    assert!(weak.upgrade().is_none());
    assert_eq!(old.total_visual(), 3);
    assert_eq!(old.logical_lookup(0).unwrap().global_visual_end, 3);
}

#[test]
fn append_truncate_and_empty_slots_preserve_coordinates() {
    let mut index = SlotIndex::default();
    index.set_slot(0, Arc::new(SlotLines::default()), &[], true);
    let (lines, map) = slot("first", 2);
    index.set_slot(1, lines, &map, true);
    index.set_slot(2, Arc::new(SlotLines::default()), &[], true);
    let (lines, map) = slot("last", 5);
    index.set_slot(3, lines, &map, true);
    assert_eq!(index.visual_lookup(0).unwrap().slot_index, 1);
    assert_eq!(index.visual_lookup(2).unwrap().slot_index, 3);
    let old = index.clone();
    index.truncate(2);
    assert_eq!(index.total_visual(), 2);
    assert_eq!(old.total_visual(), 7);
    index.truncate(0);
    let (lines, map) = slot("replacement", 4);
    index.set_slot(0, lines, &map, true);
    assert_eq!(index.total_visual(), 4);
    assert_eq!(index.slot_count(), 1);
}

#[test]
fn saturated_height_and_viewport_bounds_do_not_wrap() {
    let mut index = SlotIndex::default();
    let (lines, map) = slot("huge", usize::MAX - 1);
    index.set_slot(0, lines, &map, true);
    let (lines, map) = slot("last", 10);
    index.set_slot(1, lines, &map, true);
    assert_eq!(index.total_visual(), usize::MAX);
    assert_eq!(
        index.slot_visual_range(1),
        Some((usize::MAX - 1, usize::MAX))
    );
    assert_eq!(index.visible_slots(usize::MAX - 2, 100), 0..2);
}
