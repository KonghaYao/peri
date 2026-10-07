use super::*;
use crate::kit::entry_render_cache::EntrySurface;
use crate::kit::message_area::selection::extract_visual_range_index;
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiUserBubble};
use peri_theme::atoms::THEME_ATOM;

fn context() -> EntryRenderContext {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: crate::kit::atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

fn snapshot(count: usize) -> ViewModelsSnapshot {
    ViewModelsSnapshot {
        generation: 1,
        items: (0..count)
            .map(|slot| {
                TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("row-{slot} 中文🙂")))
            })
            .collect(),
    }
}

#[test]
#[serial_test::serial]
fn skipped_publications_do_not_rebuild_evicted_unchanged_history() {
    let mut snapshot = snapshot(256);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let grid = GridSpec::grid_for(80);
    let context = context();
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        10,
        0,
        0,
        2,
    );
    transcript.evict_to_budget(&mut caches, &(0..1), 0);
    assert!(caches[1..].iter().all(|cache| cache.lines.is_none()));
    snapshot.items.set(
        128,
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("changed in skipped generation".into())),
    );
    snapshot.items.set(
        255,
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("latest 中文🙂".into())),
    );
    snapshot.generation = 4;
    let publication = TranscriptPublication {
        generation: 4,
        previous_generation: 3,
        changed_from: 255,
    };
    let index = transcript.prepare(
        &snapshot,
        &publication,
        &mut caches,
        grid,
        grid.line_width(),
        context,
        10,
        0,
        0,
        2,
    );
    assert_eq!(transcript.scanned_slots, 256);
    assert_eq!(transcript.updated_slots, 2);
    assert_eq!(transcript.evictions, 0);
    assert!(caches[1..128].iter().all(|cache| cache.lines.is_none()));
    assert!(caches[129..255].iter().all(|cache| cache.lines.is_none()));
    let copied = extract_visual_range_index(
        &index,
        (0, 0),
        (index.total_visual().saturating_sub(1), 80),
        grid.line_width(),
        Some(&snapshot.items),
        Some(grid),
    )
    .unwrap();
    assert!(copied.contains("row-129 中文🙂"));
    assert!(copied.contains("changed in skipped generation"));
    assert!(copied.contains("latest 中文🙂"));
}

#[test]
#[serial_test::serial]
fn cold_history_layout_changes_match_fresh_height_and_semantic_copy() {
    let mut snapshot = snapshot(32);
    snapshot.items.set(
        16,
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("布局 中文🙂 words ".repeat(40))),
    );
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let initial_grid = GridSpec::grid_for(80);
    let initial_context = context();
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        initial_grid,
        initial_grid.line_width(),
        initial_context.clone(),
        10,
        0,
        0,
        2,
    );
    for grid in [GridSpec::grid_for(40), GridSpec::grid_for(40)] {
        transcript.evict_to_budget(&mut caches, &(0..0), 0);
        let mut changed_context = initial_context.clone();
        changed_context.theme = Arc::new((*initial_context.theme).clone());
        let current = transcript.prepare(
            &snapshot,
            &TranscriptPublication::default(),
            &mut caches,
            grid,
            grid.line_width(),
            changed_context.clone(),
            10,
            0,
            0,
            2,
        );
        assert_eq!(transcript.updated_slots, snapshot.items.len());
        let mut fresh = Transcript::default();
        let expected = fresh.prepare(
            &snapshot,
            &TranscriptPublication::default(),
            &mut Vec::new(),
            grid,
            grid.line_width(),
            changed_context,
            10,
            0,
            0,
            2,
        );
        assert_eq!(current.total_visual(), expected.total_visual());
        assert_eq!(current.total_logical(), expected.total_logical());
        let copied = extract_visual_range_index(
            &current,
            (0, 0),
            (current.total_visual().saturating_sub(1), 80),
            grid.line_width(),
            Some(&snapshot.items),
            Some(grid),
        );
        let expected_copy = extract_visual_range_index(
            &expected,
            (0, 0),
            (expected.total_visual().saturating_sub(1), 80),
            grid.line_width(),
            Some(&snapshot.items),
            Some(grid),
        );
        assert_eq!(copied, expected_copy);
        assert!(copied.unwrap().contains("布局 中文🙂"));
    }
}

#[test]
#[serial_test::serial]
fn immutable_render_snapshot_allows_publication_before_derivation() {
    use crate::kit::acp_events::{
        BridgeState, SessionPhase, push_view_models, push_view_models_for_reset,
    };
    use crate::kit::message_area::vm_cache::read_render_snapshot;

    let context = context();
    push_view_models_for_reset();
    let mut bridge = BridgeState {
        variant: 0,
        committed: snapshot(64).items,
        current_turn: crate::kit::acp_types::CurrentTurn::new(),
        phase: SessionPhase::Idle,
        popup_kind: None,
        generation: 0,
        active_session_id: String::new(),
        compact_just_completed: false,
        last_submitted_text: None,
        last_pushed_text_len: 0,
        last_pushed_reasoning_len: 0,
        last_successful_todos: None,
        last_successful_todo_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: Default::default(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
    };
    push_view_models(&mut bridge);
    let (captured, publication) = read_render_snapshot();
    let captured_generation = captured.generation;
    let writer = std::thread::spawn(move || {
        bridge
            .committed
            .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
                "published while deriving".into(),
            )));
        push_view_models(&mut bridge);
    });
    writer.join().unwrap();
    let (current, current_publication) = read_render_snapshot();
    assert_eq!(captured.generation, publication.generation);
    assert_eq!(captured.items.len(), 64);
    assert_eq!(current.generation, current_publication.generation);
    assert!(current.generation > captured_generation);
    assert_eq!(current.items.len(), 65);
    let grid = GridSpec::grid_for(80);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let old_index = transcript.prepare(
        &captured,
        &publication,
        &mut caches,
        grid,
        grid.line_width(),
        context,
        10,
        0,
        0,
        2,
    );
    assert_eq!(old_index.slot_count(), 64);
    push_view_models_for_reset();
    let (reset, reset_publication) = read_render_snapshot();
    assert!(reset.items.is_empty());
    assert_eq!(reset.generation, reset_publication.generation);
    transcript.evict_to_budget(&mut caches, &(0..0), 0);
    let warmed = transcript.warm_visible(&captured, &mut caches, 0, 2);
    assert_eq!(warmed.slot_count(), captured.items.len());
    assert_eq!(caches[0].content_hash, captured.items[0].content_hash());
    assert!(caches[0].lines.is_some());
}

#[test]
#[serial_test::serial]
fn warm_frames_skip_history_and_reuse_index_for_growing_histories() {
    for count in [64, 256] {
        let snapshot = snapshot(count);
        let mut transcript = Transcript::default();
        let mut caches = Vec::new();
        let grid = GridSpec::grid_for(80);
        let context = context();
        let old = transcript.prepare(
            &snapshot,
            &TranscriptPublication::default(),
            &mut caches,
            grid,
            grid.line_width(),
            context.clone(),
            10,
            0,
            0,
            12,
        );
        let warm = transcript.prepare(
            &snapshot,
            &TranscriptPublication::default(),
            &mut caches,
            grid,
            grid.line_width(),
            context,
            20,
            0,
            0,
            12,
        );
        assert!(Arc::ptr_eq(&old, &warm));
        assert_eq!(transcript.scanned_slots, 0);
        assert_eq!(transcript.updated_slots, 0);
    }
}

#[test]
#[serial_test::serial]
fn local_publication_and_same_length_replacement_keep_old_version() {
    let mut snapshot = snapshot(128);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let grid = GridSpec::grid_for(80);
    let context = context();
    let old = transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        10,
        0,
        0,
        12,
    );
    let old_text = old.materialize(127).unwrap().line(1).unwrap().to_string();
    snapshot.items.set(
        127,
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("replacement".into())),
    );
    snapshot.generation = 2;
    let publication = TranscriptPublication {
        generation: 2,
        previous_generation: 1,
        changed_from: 127,
    };
    let current = transcript.prepare(
        &snapshot,
        &publication,
        &mut caches,
        grid,
        grid.line_width(),
        context,
        10,
        0,
        0,
        12,
    );
    assert_eq!(transcript.scanned_slots, 1);
    assert_eq!(
        old.materialize(127).unwrap().line(1).unwrap().to_string(),
        old_text
    );
    assert!(
        current
            .materialize(127)
            .unwrap()
            .line(1)
            .unwrap()
            .to_string()
            .contains("replacement")
    );
}

#[test]
#[serial_test::serial]
fn bridge_publication_after_local_fold_updates_resident_and_appended_entries() {
    use crate::kit::acp_events::{BridgeState, SessionPhase, push_view_models};
    use crate::kit::acp_types::CurrentTurn;
    use crate::kit::atoms::{TRANSCRIPT_PUBLICATION, VIEW_MODELS};
    use crate::kit::tui_render_unit::{EntryStatus, FoldState, TuiReasoningBlock};

    let context = context();
    crate::kit::acp_events::push_view_models_for_reset();
    let mut bubble = TuiAssistantBubble {
        text: "before".into(),
        reasoning: Some(TuiReasoningBlock {
            text: "reasoning body".into(),
            fold: FoldState::Collapsed,
            status: EntryStatus::Completed,
            is_running: false,
            started_at: None,
            duration_ms: Some(1000),
        }),
        message_id: Some("fold-collision".into()),
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    let mut bridge = BridgeState {
        variant: 0,
        committed: im::Vector::from(vec![TuiRenderUnit::TuiAssistantBubble(Arc::new(bubble))]),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
        popup_kind: None,
        generation: 0,
        active_session_id: String::new(),
        compact_just_completed: false,
        last_submitted_text: None,
        last_pushed_text_len: 0,
        last_pushed_reasoning_len: 0,
        last_successful_todos: None,
        last_successful_todo_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: Default::default(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
    };
    let grid = GridSpec::grid_for(80);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    push_view_models(&mut bridge);
    for fold_count in [1, 2] {
        for _ in 0..fold_count {
            let view_models = VIEW_MODELS.state();
            let mut snapshot = view_models.write();
            assert_eq!(
                super::super::entry_nav::apply_fold_toggle(&mut snapshot, 0, false),
                ratatui_kit::prelude::EventResult::Consumed,
            );
        }
        let folded = VIEW_MODELS.state().read().clone();
        transcript.prepare(
            &folded,
            &TRANSCRIPT_PUBLICATION.get(),
            &mut caches,
            grid,
            grid.line_width(),
            context.clone(),
            10,
            0,
            0,
            100,
        );
        let TuiRenderUnit::TuiAssistantBubble(bubble) = &bridge.committed[0] else {
            panic!("expected assistant bubble");
        };
        let mut replacement = bubble.as_ref().clone();
        replacement.text = format!("updated-after-{fold_count}");
        replacement.recompute_hash();
        bridge
            .committed
            .set(0, TuiRenderUnit::TuiAssistantBubble(Arc::new(replacement)));
        bridge
            .committed
            .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!(
                "appended-{fold_count}"
            ))));
        push_view_models(&mut bridge);
        let published = VIEW_MODELS.state().read().clone();
        assert!(published.generation > folded.generation);
        let index = transcript.prepare(
            &published,
            &TRANSCRIPT_PUBLICATION.get(),
            &mut caches,
            grid,
            grid.line_width(),
            context.clone(),
            10,
            0,
            0,
            100,
        );
        assert_eq!(index.slot_count(), published.items.len());
        let copied = extract_visual_range_index(
            &index,
            (0, 0),
            (index.total_visual().saturating_sub(1), 80),
            grid.line_width(),
            Some(&published.items),
            Some(grid),
        )
        .expect("complete transcript copy");
        assert!(copied.contains(&format!("updated-after-{fold_count}")));
        assert!(copied.contains(&format!("appended-{fold_count}")));
    }
    crate::kit::acp_events::push_view_models_for_reset();
}

#[test]
#[serial_test::serial]
fn eviction_releases_old_frame_payload_and_cold_unicode_copy_matches_warm() {
    let context = context();
    let text = "# 中文🙂\n\n- first item\n- 第二项\n\n```rust\nlet answer = 42;\n```\n\n| left | right |\n| --- | --- |\n| 中文 | 🙂 |";
    let mut bubble = TuiAssistantBubble {
        text: text.into(),
        reasoning: None,
        message_id: None,
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    let snapshot = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiAssistantBubble(Arc::new(bubble))]),
        generation: 1,
    };
    let grid = GridSpec::grid_for(60);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let index = transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        10,
        0,
        0,
        4,
    );
    let old_lines = Arc::downgrade(caches[0].lines.as_ref().unwrap());
    let end = index.total_visual().saturating_sub(1);
    let warm = extract_visual_range_index(
        &index,
        (0, 0),
        (end, 60),
        grid.line_width(),
        Some(&snapshot.items),
        Some(grid),
    );
    transcript.evict_to_budget(&mut caches, &(0..0), 0);
    assert!(
        old_lines.upgrade().is_none(),
        "old index must not retain heavy payload"
    );
    assert_eq!(transcript.retained_bytes, 0);
    assert!(caches[0].wrap_map.is_empty());
    let cold = extract_visual_range_index(
        &index,
        (0, 0),
        (end, 60),
        grid.line_width(),
        Some(&snapshot.items),
        Some(grid),
    );
    assert_eq!(cold, warm);
    assert_eq!(
        transcript.retained_bytes, 0,
        "copy does not populate permanent cache"
    );
}

#[test]
#[serial_test::serial]
fn visible_oversize_exception_and_reset_are_explicit() {
    let context = context();
    let snapshot = snapshot(16);
    let grid = GridSpec::grid_for(80);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        10,
        0,
        0,
        2,
    );
    transcript.evict_to_budget(&mut caches, &(0..1), 0);
    assert!(caches[0].lines.is_some());
    assert!(caches[1..].iter().all(|cache| cache.lines.is_none()));
    assert_eq!(transcript.retained_bytes, caches[0].retained_bytes());
    let empty = transcript.prepare(
        &ViewModelsSnapshot::default(),
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        10,
        1,
        0,
        2,
    );
    assert_eq!(empty.total_visual(), 0);
    assert_eq!(transcript.retained_bytes, 0);
    assert!(caches.is_empty());
}
