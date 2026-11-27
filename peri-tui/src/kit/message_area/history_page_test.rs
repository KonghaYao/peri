use super::{
    Arc, GridSpec, Transcript, TranscriptPublication, TuiRenderUnit, TuiUserBubble, context,
    snapshot, tool_entry,
};

#[test]
#[serial_test::serial]
fn history_page_preserves_cold_geometry_hot_lines_and_relocates_after_release() {
    let mut snap = snapshot(192);
    let grid = GridSpec::grid_for(54);
    let context = context();
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let original_epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get();
    let original_achieved = crate::kit::atoms::RENDER_ACHIEVED_EPOCH.get();
    snap.items.set(0, tool_entry("first tool"));
    snap.items.set(
        72,
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("wrapped 中文🙂 ".repeat(30))),
    );
    let old = transcript.prepare(
        &snap,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        1,
        0,
        0,
        2,
    );
    let from = old.slot_visual_start(72).unwrap() + 2;
    transcript.evict_to_budget(&mut caches, &(72..73), 0);
    let hot = Arc::downgrade(caches[72].lines.as_ref().unwrap());
    assert!(caches[0].lines.is_none());
    // A budget release drops the far end in the same publication that adds older messages.
    snap.items.split_off(150);
    snap.items
        .push_front(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            String::new(),
        )));
    snap.items
        .push_front(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "older".into(),
        )));
    snap.generation += 1;
    crate::kit::atoms::STRUCTURAL_EPOCH.set(original_epoch + 1);
    let new = transcript.prepare(
        &snap,
        &TranscriptPublication {
            prepend_epoch: 1,
            structural_epoch: original_epoch + 1,
            ..Default::default()
        },
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        2,
        0,
        from,
        2,
    );
    assert_eq!(new.slot_count(), snap.items.len());
    assert_eq!(
        transcript.scanned_slots, 3,
        "the page and its first emitting neighbor"
    );
    assert_eq!(
        transcript.updated_slots, 3,
        "cold seam gained a process step; old interior stayed cold"
    );
    assert!(Arc::ptr_eq(
        &hot.upgrade().unwrap(),
        caches[74].lines.as_ref().unwrap()
    ));
    assert!(caches[3..74].iter().all(|slot| slot.lines.is_none()));
    assert_eq!(
        new.slot_visual_start(74).unwrap() + 2,
        transcript.take_prepend_relocation().unwrap().target_offset
    );
    assert_eq!(old.slot_count(), 192);
    assert_eq!(old.visual_lookup(from).unwrap().slot_index, 72);
    let mut fresh = Transcript::default();
    let fresh_index = fresh.prepare(
        &snap,
        &TranscriptPublication {
            prepend_epoch: 1,
            structural_epoch: original_epoch + 1,
            ..Default::default()
        },
        &mut Vec::new(),
        grid,
        grid.line_width(),
        context.clone(),
        2,
        0,
        0,
        2,
    );
    for i in 0..snap.items.len() {
        assert_eq!(new.slot_visual_range(i), fresh_index.slot_visual_range(i));
    }
    assert_eq!(new.line(30, 0), fresh_index.line(30, 0));
    // Another prepend crosses the tree's old left bound. A subsequent live update must hit
    // exactly the new tail, with all indices and byte counters still belonging to that slot.
    snap.items
        .push_front(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "oldest".into(),
        )));
    snap.generation += 1;
    transcript.prepare(
        &snap,
        &TranscriptPublication {
            prepend_epoch: 2,
            structural_epoch: original_epoch + 1,
            ..Default::default()
        },
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        3,
        0,
        0,
        1,
    );
    assert_eq!(transcript.scanned_slots, 2);
    let tail = snap.items.len() - 1;
    snap.items.set(
        tail,
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("new live tail".into())),
    );
    snap.generation += 1;
    let publication = TranscriptPublication {
        generation: snap.generation,
        previous_generation: snap.generation - 1,
        changed_from: tail,
        prepend_epoch: 2,
        structural_epoch: original_epoch + 1,
        ..Default::default()
    };
    let live = transcript.prepare(
        &snap,
        &publication,
        &mut caches,
        grid,
        grid.line_width(),
        context,
        4,
        0,
        0,
        1,
    );
    assert_eq!(transcript.scanned_slots, 1);
    assert!(
        live.line(tail, 0)
            .unwrap()
            .to_string()
            .contains("new live tail")
    );
    assert_eq!(
        transcript.bytes.iter().sum::<usize>(),
        transcript.retained_bytes
    );
    crate::kit::atoms::STRUCTURAL_EPOCH.set(original_epoch);
    crate::kit::atoms::RENDER_ACHIEVED_EPOCH.set(original_achieved);
}

#[test]
#[serial_test::serial]
fn history_page_worker_measures_exact_markdown_and_invalidates_on_resize() {
    use crate::kit::atoms::{HISTORY_PREPEND_EPOCH, VIEW_MODELS};
    use crate::kit::message_area::history_layout;
    let context = context();
    crate::i18n::init(Some("zh-CN"));
    let grid = GridSpec::grid_for(73);
    let epoch = HISTORY_PREPEND_EPOCH.get();
    let prior = VIEW_MODELS.state().read().clone();
    let prior_publication = crate::kit::atoms::TRANSCRIPT_PUBLICATION.get();
    let mut base = snapshot(20);
    base.generation = 523;
    VIEW_MODELS.set(base.clone());
    let mut caches = Vec::new();
    let mut transcript = Transcript::default();
    let old = transcript.prepare(
        &base,
        &TranscriptPublication {
            prepend_epoch: epoch,
            ..Default::default()
        },
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        3,
        0,
        0,
        1,
    );
    let old_y = old.slot_visual_start(10).unwrap();
    let mut next = base.clone();
    next.generation += 1;
    next.items.push_front(super::empty_bubble_entry(
        "## 中文\n\n- wrapped 正文测试\n\n```rust\nfn draw() {}\n```\n"
            .repeat(10)
            .as_str(),
        None,
    ));
    HISTORY_PREPEND_EPOCH.set(epoch.wrapping_add(1));
    let worker = || {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::i18n::init(Some("en"));
                    let page = history_layout::measure_page(base.generation, &next.items);
                    assert_eq!(
                        crate::i18n::current_language(),
                        "en",
                        "worker locale restored"
                    );
                    page.expect("bridge measured frozen Markdown")
                })
                .join()
                .unwrap()
        })
    };
    let for_reset = worker();
    let for_resize = worker();
    let for_display = worker();
    history_layout::publish(next.generation, Some(for_display));
    let page = transcript.prepare(
        &next,
        &TranscriptPublication {
            prepend_epoch: epoch.wrapping_add(1),
            ..Default::default()
        },
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        4,
        0,
        old_y,
        1,
    );
    assert!(caches[0].lines.is_none());
    assert_eq!(
        transcript.updated_slots, 0,
        "no foreground rendering of offscreen Markdown"
    );
    let mut fresh = Transcript::default();
    let reference = fresh.prepare(
        &next,
        &TranscriptPublication {
            prepend_epoch: epoch.wrapping_add(1),
            ..Default::default()
        },
        &mut Vec::new(),
        grid,
        grid.line_width(),
        context.clone(),
        4,
        0,
        0,
        1,
    );
    assert_eq!(page.slot_visual_range(0), reference.slot_visual_range(0));
    assert_eq!(page.total_logical(), reference.total_logical());
    assert_eq!(page.line(0, 4), reference.line(0, 4));
    let visible = transcript.warm_visible(&next, &mut caches, 0, 8);
    assert!(caches[0].lines.is_some());
    assert_eq!(visible.total_visual(), page.total_visual());
    history_layout::publish(next.generation, Some(for_resize));
    let narrow = GridSpec::grid_for(41);
    assert!(
        history_layout::begin_frame(
            next.generation,
            epoch.wrapping_add(1),
            narrow,
            narrow.line_width(),
            &context
        )
        .is_none()
    );
    history_layout::publish(next.generation, Some(for_reset));
    assert_eq!(history_layout::retained_for_test(), (true, true));
    crate::kit::acp_events::push_view_models_for_reset();
    assert_eq!(history_layout::retained_for_test(), (false, false));
    HISTORY_PREPEND_EPOCH.set(epoch);
    VIEW_MODELS.set(prior);
    crate::kit::atoms::TRANSCRIPT_PUBLICATION.set(prior_publication);
    crate::i18n::init(Some("en"));
}

#[test]
#[serial_test::serial]
fn history_page_epoch_then_local_fold_then_bridge_commit_preserves_anchor_and_release() {
    use crate::kit::atoms::{HISTORY_PREPEND_EPOCH, RENDER_ACHIEVED_EPOCH, STRUCTURAL_EPOCH};
    let mut snap = snapshot(32);
    let mut caches = Vec::new();
    let mut transcript = Transcript::default();
    let ctx = context();
    let grid = GridSpec::grid_for(59);
    let old_epoch = HISTORY_PREPEND_EPOCH.get();
    let old_release = STRUCTURAL_EPOCH.get();
    let old_ack = RENDER_ACHIEVED_EPOCH.get();
    let mut pub_meta = TranscriptPublication {
        generation: snap.generation,
        prepend_epoch: old_epoch,
        structural_epoch: old_release,
        ..Default::default()
    };
    let old = transcript.prepare(
        &snap,
        &pub_meta,
        &mut caches,
        grid,
        grid.line_width(),
        ctx.clone(),
        1,
        0,
        0,
        1,
    );
    let from = old.slot_visual_start(5).unwrap();
    let kept = Arc::downgrade(caches[5].lines.as_ref().unwrap());
    let ack_before_page = RENDER_ACHIEVED_EPOCH.get();
    HISTORY_PREPEND_EPOCH.set(old_epoch.wrapping_add(1));
    STRUCTURAL_EPOCH.set(old_release + 1);
    // Fold locally after the bridge announces its page but before it publishes. Only the
    // snapshot's generation changes. The last bridge metadata still describes the old page.
    snap.items
        .set(20, tool_entry("folded during page measurement"));
    snap.generation += 1;
    transcript.prepare(
        &snap,
        &pub_meta,
        &mut caches,
        grid,
        grid.line_width(),
        ctx.clone(),
        2,
        0,
        from,
        1,
    );
    assert!(kept.upgrade().is_some());
    assert!(transcript.take_prepend_relocation().is_none());
    assert_eq!(transcript.merge_epoch_seen, old_epoch);
    assert_eq!(RENDER_ACHIEVED_EPOCH.get(), ack_before_page);
    snap.items.pop_back();
    snap.items
        .push_front(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "earlier".into(),
        )));
    pub_meta.previous_generation = snap.generation;
    snap.generation += 1;
    pub_meta.generation = snap.generation;
    pub_meta.prepend_epoch = HISTORY_PREPEND_EPOCH.get();
    pub_meta.structural_epoch = STRUCTURAL_EPOCH.get();
    let after = transcript.prepare(
        &snap,
        &pub_meta,
        &mut caches,
        grid,
        grid.line_width(),
        ctx.clone(),
        3,
        0,
        from,
        1,
    );
    let moved = transcript
        .take_prepend_relocation()
        .expect("bridge page keeps the unique anchor");
    assert_eq!(moved.target_offset, after.slot_visual_start(6).unwrap());
    assert!(moved.target_offset > from);
    assert_eq!(RENDER_ACHIEVED_EPOCH.get(), old_ack.max(old_release + 1));
    snap.items.push_front(snap.items[6].clone());
    snap.generation += 1;
    pub_meta.generation = snap.generation;
    pub_meta.prepend_epoch = pub_meta.prepend_epoch.wrapping_add(1);
    transcript.prepare(
        &snap,
        &pub_meta,
        &mut caches,
        grid,
        grid.line_width(),
        ctx,
        4,
        0,
        moved.target_offset,
        1,
    );
    assert!(
        transcript.take_prepend_relocation().is_none(),
        "the next page adds an ambiguous anchor"
    );
    HISTORY_PREPEND_EPOCH.set(old_epoch);
    STRUCTURAL_EPOCH.set(old_release);
    RENDER_ACHIEVED_EPOCH.set(old_ack);
}
