use super::*;
use crate::kit::entry_render_cache::EntrySurface;
use crate::kit::message_area::selection::extract_visual_range_index;
use crate::kit::tui_render_unit::{EntryStatus, FoldState, TuiToolCard};
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiReasoningBlock, TuiUserBubble};
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
    // 2 个内容变化的 slot（128 / 255）+ 1 个视口补建：user 行高 1 行，
    // height=2 的视口覆盖 slot 0..=1，slot 1 虽未被内容改动但已淘汰、需补建。
    assert_eq!(transcript.updated_slots, 3);
    assert_eq!(transcript.evictions, 0);
    // slot 1 在 height=2 视口内被补建（user 行高 1 行，视口覆盖 0..=1）；
    // 其余淘汰历史保持未重建。
    assert!(caches[2..128].iter().all(|cache| cache.lines.is_none()));
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
    let old_text = old.materialize(127).unwrap().line(0).unwrap().to_string();
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
        old.materialize(127).unwrap().line(0).unwrap().to_string(),
        old_text
    );
    assert!(
        current
            .materialize(127)
            .unwrap()
            .line(0)
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

/// [回归测试] Resize 时 grid 先更新，tracker 下一帧才返回实际宽度；
/// 后续仅宽度修正也必须更新视觉索引，否则正文提前裁断并留下大片空白。
#[test]
#[serial_test::serial]
fn test_viewport_width_correction_rebuilds_visual_index() {
    let mut snapshot = snapshot(1);
    snapshot.items.set(
        0,
        TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("中文 viewport words ".repeat(50))),
    );
    let grid = GridSpec::grid_for(100);
    let context = context();
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        35,
        context.clone(),
        0,
        0,
        0,
        20,
    );
    let corrected = transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        0,
        0,
        0,
        20,
    );
    let fresh = Transcript::default().prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut Vec::new(),
        grid,
        grid.line_width(),
        context,
        0,
        0,
        0,
        20,
    );
    assert_eq!(
        corrected.total_visual(),
        fresh.total_visual(),
        "视宽收敛后不能保留旧折行高度"
    );
    assert_eq!(
        corrected.viewport_logical_range(0, 20),
        fresh.viewport_logical_range(0, 20)
    );
}

/// 单行 header 的 completed 工具卡（过程区 entry）。
fn tool_entry(name: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: format!("tc-{name}"),
        tool_name: name.to_string(),
        input_summary: name.to_string(),
        output_summary: "done".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(400),
        diff: None,
        presentation: Default::default(),
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 5,
        tool_calls_count: 0,
    })
}

/// 无正文无 reasoning 的空气泡——渲染 0 行，不参与相邻判定（§7）。
fn empty_bubble_entry(text: &str, reasoning: Option<TuiReasoningBlock>) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: text.to_string(),
            reasoning,
            message_id: None,
            content_hash: 9,
        }
        .into(),
    )
}

/// §6 转场台阶只出现在区界：文本区 → 过程区在 entry 入口画进入台阶 `╰─╮`，
/// 过程区 → 文本区画退出台阶 `╭─╯`；首个 entry 无台阶（无悬空台阶），
/// 同区接续不重复画。
#[test]
#[serial_test::serial]
fn zone_step_renders_only_at_zone_boundaries() {
    let grid = GridSpec::grid_for(80);
    let context = context();
    let snapshot = ViewModelsSnapshot {
        generation: 1,
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("hi".into())),
            tool_entry("Read"),
            tool_entry("Grep"),
            // 长回复（≥120 字）→ 文本区，参与转场
            empty_bubble_entry(&"answer ".repeat(20), None),
            tool_entry("Bash"),
        ]),
    };
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        0,
        0,
        0,
        2,
    );

    let line_text_at = |slot: usize, index: usize| -> String {
        let lines = caches[slot].lines.as_ref().expect("slot 已渲染");
        let line = lines.line(index).expect("行存在");
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    };
    let first_line = |slot: usize| line_text_at(slot, 0);
    // 首个 entry = user 正文（文本区），内部无台阶
    assert!(
        !first_line(0).contains('\u{2570}') && !first_line(0).contains('\u{256d}'),
        "首个 entry 不画台阶，实际 {:?}",
        first_line(0)
    );
    // 文本区 → 过程区：入口台阶独立成行，header 在次行（标记列 ✓）
    assert_eq!(first_line(1), " \u{2570}\u{2500}\u{256e}");
    let header = line_text_at(1, 1);
    assert!(header.starts_with("   \u{2713}"), "实际 {header:?}");
    assert!(header.contains("Read"), "实际 {header:?}");
    // 连续过程 entry：同区接续，行首即 header（无台阶）
    let inline = first_line(2);
    assert!(inline.starts_with("   \u{2713}"), "实际 {inline:?}");
    assert!(inline.contains("Grep"), "实际 {inline:?}");
    // 过程区 → 文本区：退出台阶独立成行
    assert_eq!(first_line(3), " \u{256d}\u{2500}\u{256f}");
    // 文本区 → 过程区：再次进入画台阶
    assert_eq!(first_line(4), " \u{2570}\u{2500}\u{256e}");
    let header = line_text_at(4, 1);
    assert!(header.contains("Bash"), "实际 {header:?}");
}

/// §6/§7 短回复（<120 字）并入过程区：与相邻过程 entry 同区接续，双向都不画
/// 转场台阶；正文行取过程带前缀（缩进 + 标记列竖线）。
#[test]
#[serial_test::serial]
fn brief_reply_stays_in_process_zone_without_step() {
    let grid = GridSpec::grid_for(80);
    let context = context();
    let snapshot = ViewModelsSnapshot {
        generation: 1,
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("hi".into())),
            tool_entry("Read"),
            empty_bubble_entry("answer", None),
            tool_entry("Bash"),
        ]),
    };
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        0,
        0,
        0,
        2,
    );

    let line_text_at = |slot: usize, index: usize| -> String {
        let lines = caches[slot].lines.as_ref().expect("slot 已渲染");
        let line = lines.line(index).expect("行存在");
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    };
    // user → 过程区（Read）：进入台阶独立成行
    assert_eq!(line_text_at(1, 0), " \u{2570}\u{2500}\u{256e}");
    // Read → 短回复：同区接续，无退出台阶；正文取过程带前缀，上下各 1 过程带空行
    assert_eq!(line_text_at(2, 0), "   \u{2502}  ");
    assert_eq!(line_text_at(2, 1), "   \u{2502}  answer");
    assert_eq!(line_text_at(2, 2), "   \u{2502}  ");
    // 短回复 → Bash：仍同区接续，无进入台阶
    let bash_head = line_text_at(3, 0);
    assert!(
        !bash_head.contains('\u{2570}') && !bash_head.contains('\u{256d}'),
        "短回复后接过程 entry 不画台阶，实际 {bash_head:?}"
    );
    assert!(bash_head.contains("Bash"), "实际 {bash_head:?}");
}

/// §6/§7 相邻判定的边界用例：首个 entry 恒 `None`；0 行 entry 跳过（不成为
/// 「前一个产出行 entry」）；纯 reasoning 气泡终点仍在过程区 → 接续不画台阶；
/// 短回复（<120 字）气泡同属过程区 → 双向都不画台阶；长正文气泡终点回文本区
/// → 后接过程 entry 画进入台阶。
#[test]
#[serial_test::serial]
fn zone_transition_matrix_skips_zero_line_entries() {
    let long_answer = || empty_bubble_entry(&"answer ".repeat(20), None);
    let brief_answer = || empty_bubble_entry("answer", None);
    let reasoning_only = || {
        empty_bubble_entry(
            "",
            Some(TuiReasoningBlock {
                text: "思考".into(),
                fold: FoldState::Collapsed,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: Some(1_000),
            }),
        )
    };
    let user = || TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("hi".into()));

    // 首个 entry 起点即过程区 → None（不画悬空台阶）
    let items = im::Vector::from(vec![tool_entry("Read")]);
    assert_eq!(zone_transition(&items, 0), ZoneTransition::None);

    // 文本区 → 过程区 → 过程区：进入处台阶（线色随上方 user 文本），区内接续
    let items = im::Vector::from(vec![user(), tool_entry("Read"), tool_entry("Grep")]);
    assert_eq!(
        zone_transition(&items, 1),
        ZoneTransition::EnterProcess(TextLineSource::User)
    );
    assert_eq!(zone_transition(&items, 2), ZoneTransition::None);

    // 过程区 → 文本区：长正文退出台阶（线色随下方 assistant 正文）
    let items = im::Vector::from(vec![tool_entry("Read"), long_answer()]);
    assert_eq!(
        zone_transition(&items, 1),
        ZoneTransition::ExitProcess(TextLineSource::Assistant)
    );

    // 短回复终点仍在过程区：与前置过程 entry 接续，不画退出台阶
    let items = im::Vector::from(vec![tool_entry("Read"), brief_answer()]);
    assert_eq!(zone_transition(&items, 1), ZoneTransition::None);

    // 0 行 entry 夹在中间：不参与相邻判定，跨过它看前一个产出行 entry
    let items = im::Vector::from(vec![
        user(),
        empty_bubble_entry("", None),
        tool_entry("Read"),
    ]);
    assert_eq!(
        zone_transition(&items, 2),
        ZoneTransition::EnterProcess(TextLineSource::User)
    );

    // 纯 reasoning 气泡（起点过程 / 终点过程）→ 后接过程 entry 不画台阶
    let items = im::Vector::from(vec![user(), reasoning_only(), tool_entry("Read")]);
    assert_eq!(zone_transition(&items, 2), ZoneTransition::None);

    // 长正文气泡终点在文本区 → 后接过程 entry 画进入台阶（线色随上方 assistant 正文）
    let items = im::Vector::from(vec![user(), long_answer(), tool_entry("Read")]);
    assert_eq!(
        zone_transition(&items, 2),
        ZoneTransition::EnterProcess(TextLineSource::Assistant)
    );

    // 短回复气泡终点仍在过程区 → 后接过程 entry 接续，不画进入台阶
    let items = im::Vector::from(vec![user(), brief_answer(), tool_entry("Read")]);
    assert_eq!(zone_transition(&items, 2), ZoneTransition::None);

    // turn 边界 Divider（文本区装饰）：进出两向台阶线色都随它自身（text.dim）
    let divider = || {
        TuiRenderUnit::TuiDivider(crate::kit::tui_render_unit::TuiDivider {
            label: None,
            content_hash: 9,
        })
    };
    let items = im::Vector::from(vec![tool_entry("Read"), divider()]);
    assert_eq!(
        zone_transition(&items, 1),
        ZoneTransition::ExitProcess(TextLineSource::Divider)
    );
    let items = im::Vector::from(vec![divider(), tool_entry("Read")]);
    assert_eq!(
        zone_transition(&items, 1),
        ZoneTransition::EnterProcess(TextLineSource::Divider)
    );
}

/// §6 首个 entry 即过程区（会话以工具卡开场）：完整渲染路径下首行是过程区
/// 首行形态（标记列状态符号），不画悬空台阶；后续过程 entry 同样接续。
#[test]
#[serial_test::serial]
fn first_process_entry_renders_without_dangling_step() {
    let grid = GridSpec::grid_for(80);
    let context = context();
    let snapshot = ViewModelsSnapshot {
        generation: 1,
        items: im::Vector::from(vec![tool_entry("Read"), tool_entry("Grep")]),
    };
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        0,
        0,
        0,
        2,
    );

    let first_line = |slot: usize| -> String {
        let lines = caches[slot].lines.as_ref().expect("slot 已渲染");
        let line = lines.line(0).expect("首行存在");
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    };
    let head = first_line(0);
    assert!(
        head.starts_with("   \u{2713}"),
        "首个过程 entry 首行形态，实际 {head:?}"
    );
    assert!(
        !head.contains('\u{2570}') && !head.contains('\u{256d}'),
        "无悬空台阶，实际 {head:?}"
    );
    assert!(head.contains("Read"), "实际 {head:?}");
    let next = first_line(1);
    assert!(
        next.starts_with("   \u{2713}"),
        "区内接续不重复画台阶，实际 {next:?}"
    );
    assert!(next.contains("Grep"), "实际 {next:?}");
}
