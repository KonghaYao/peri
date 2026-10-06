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
