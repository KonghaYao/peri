use std::sync::Arc;

use super::*;
use crate::kit::entry_render_cache::EntrySurface;
use crate::kit::tui_render_unit::TuiAssistantBubble;

#[test]
#[serial_test::serial]
fn genui_semantic_copy_skips_chrome_in_resident_and_cold_history() {
    crate::kit::atoms::init_atoms();
    let text = "before\n\n```gen-ui\nroot = Stack([Card([Text(\"只渲染不复制\"), Button(\"继续\", @ToAssistant(\"next\"))], \"标题\")])\n```\n\nafter";
    let mut bubble = TuiAssistantBubble {
        text: text.into(),
        reasoning: None,
        message_id: None,
        started_at: Some(peri_time::monotonic_now()),
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    let vm = TuiRenderUnit::TuiAssistantBubble(Arc::new(bubble));
    let snapshot = ViewModelsSnapshot {
        generation: 1,
        items: vec![vm].into_iter().collect(),
    };
    let theme_state = peri_theme::atoms::THEME_ATOM.state();
    let context = EntryRenderContext {
        theme: Arc::clone(&theme_state.read()),
        language: 0,
        surface: EntrySurface::Message,
        occurrence: 0,
    };
    let grid = GridSpec::grid_for(80);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let index = transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        0,
        0,
        0,
        80,
    );
    let rendered = index.materialize(0).unwrap();
    let all_text: String = (0..rendered.len())
        .map(|index| rendered.line(index).unwrap().to_string())
        .collect();
    assert!(all_text.contains('┌'));
    assert!(all_text.contains("只渲染不复制"));
    assert_eq!(caches[0].genui_buttons.len(), 1);
    let copy = |index: &SlotIndex| {
        super::super::selection::extract_visual_range_index(
            index,
            (0, 0),
            (index.total_visual().saturating_sub(1), 80),
            grid.line_width(),
            Some(&snapshot.items),
            Some(grid),
        )
        .unwrap()
    };
    let resident = copy(&index);
    drop(rendered);
    drop(index);
    transcript.evict_to_budget(&mut caches, &(0..0), 0);
    assert!(caches[0].lines.is_none());
    assert!(caches[0].genui_buttons.is_empty());
    let cold = copy(&transcript.index);
    assert_eq!(resident, cold);
    assert!(cold.contains("before"));
    assert!(cold.contains("after"));
    assert!(!cold.contains("只渲染不复制"));
    assert!(!cold.contains("继续"));
    assert!(
        !cold
            .chars()
            .any(|character| matches!(character, '┌' | '└' | '─' | '│'))
    );
}
