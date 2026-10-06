use super::*;
use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAssistantBubble, TuiNoteLevel, TuiReasoningBlock, TuiSystemNote,
};

fn bubble(text: &str) -> TuiRenderUnit {
    let mut value = TuiAssistantBubble {
        text: text.to_string(),
        reasoning: None,
        message_id: None,
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    value.recompute_hash();
    TuiRenderUnit::TuiAssistantBubble(Arc::new(value))
}

fn context(surface: EntrySurface) -> EntryRenderContext {
    EntryRenderContext {
        theme: Arc::new(peri_theme::builtin::dark_theme()),
        language: 0,
        surface,
        occurrence: 1,
    }
}

#[test]
#[serial_test::serial]
fn unchanged_entry_has_no_parse_or_wrap_work() {
    let mut cache = EntryRenderCache::default();
    let vm = bubble("# cached body");
    let grid = GridSpec::with_content(60);
    let context = context(EntrySurface::Message);
    assert_eq!(
        cache.ensure(&vm, &grid, context.clone(), 0),
        EntryInvalidation::Content
    );
    let lines = Arc::clone(cache.lines());
    reset_perf_counters();
    assert_eq!(
        cache.ensure(&vm, &grid, context, 100),
        EntryInvalidation::None
    );
    assert!(Arc::ptr_eq(&lines, cache.lines()));
    assert_eq!(perf_counters().full_parses, 0);
    assert_eq!(perf_counters().wrap_recalculated_lines, 0);
}

#[test]
#[serial_test::serial]
fn complete_grid_theme_language_and_occurrence_invalidate_separately() {
    let mut cache = EntryRenderCache::default();
    let vm = bubble("# cached body");
    let mut grid = GridSpec::with_content(60);
    let mut context = context(EntrySurface::Detail);
    cache.ensure(&vm, &grid, context.clone(), 0);
    grid.gap = grid.gap.saturating_add(1);
    assert_eq!(
        cache.ensure(&vm, &grid, context.clone(), 0),
        EntryInvalidation::Layout
    );
    context.language = 1;
    assert_eq!(
        cache.ensure(&vm, &grid, context.clone(), 0),
        EntryInvalidation::Layout
    );
    context.theme = Arc::new(peri_theme::builtin::light_theme());
    assert_eq!(
        cache.ensure(&vm, &grid, context.clone(), 0),
        EntryInvalidation::Layout
    );
    context.occurrence = 2;
    assert_eq!(
        cache.ensure(&vm, &grid, context, 0),
        EntryInvalidation::Content
    );
}

#[test]
#[serial_test::serial]
fn surface_controls_copy_button_without_sharing_foreign_layout() {
    let mut cache = EntryRenderCache::default();
    let text = "copyable markdown text ".repeat(30);
    let vm = bubble(&text);
    let grid = GridSpec::with_content(80);
    let mut context = context(EntrySurface::Message);
    cache.ensure(&vm, &grid, context.clone(), 0);
    assert!(cache.copy_button().is_some());
    context.surface = EntrySurface::Detail;
    assert_eq!(
        cache.ensure(&vm, &grid, context, 0),
        EntryInvalidation::Content
    );
    assert!(cache.copy_button().is_none());
    let resolved = (0..cache.lines().len())
        .filter_map(|index| {
            cache.line(index).and_then(|line| {
                crate::kit::message_area::render::semantic_line_text(&vm, index, line, &grid)
            })
        })
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(
        resolved
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>(),
        text.chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
    );
}

#[test]
#[serial_test::serial]
fn same_hash_different_variant_cannot_hit_previous_payload() {
    let mut cache = EntryRenderCache::default();
    let vm = bubble("assistant");
    let grid = GridSpec::with_content(60);
    let context = context(EntrySurface::Detail);
    cache.ensure(&vm, &grid, context.clone(), 0);
    let replacement = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "system replacement".into(),
        level: TuiNoteLevel::Info,
        content_hash: vm.content_hash(),
    });
    assert_eq!(
        cache.ensure(&replacement, &grid, context, 0),
        EntryInvalidation::Content
    );
    assert!(
        cache
            .lines()
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.content.contains("system replacement"))
    );
}

#[test]
#[serial_test::serial]
fn reasoning_animation_only_refreshes_header_without_markdown_or_wrap() {
    let mut cache = EntryRenderCache::default();
    let TuiRenderUnit::TuiAssistantBubble(mut value) = bubble(&"body text ".repeat(100)) else {
        unreachable!()
    };
    Arc::make_mut(&mut value).reasoning = Some(TuiReasoningBlock {
        text: "reasoning body".into(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(peri_time::monotonic_now()),
        duration_ms: None,
    });
    Arc::make_mut(&mut value).recompute_hash();
    let vm = TuiRenderUnit::TuiAssistantBubble(value);
    let context = context(EntrySurface::Detail);
    let grid = GridSpec::with_content(60);
    cache.ensure(&vm, &grid, context.clone(), 0);
    let body = cache.lines()[1..].to_vec();
    reset_perf_counters();
    assert_eq!(
        cache.ensure(&vm, &grid, context, 10),
        EntryInvalidation::Animation
    );
    assert_eq!(&cache.lines()[1..], body.as_slice());
    assert_eq!(perf_counters().full_parses, 0);
    assert_eq!(perf_counters().wrap_recalculated_lines, 0);
}

#[test]
#[serial_test::serial]
fn stable_chunks_use_visible_line_access_and_resize_reuses_parsed_blocks() {
    let mut cache = EntryRenderCache::default();
    let TuiRenderUnit::TuiAssistantBubble(mut value) =
        bubble("# stable\n\nparagraph one\n\nparagraph two\n\ntail")
    else {
        unreachable!()
    };
    Arc::make_mut(&mut value).started_at = Some(peri_time::monotonic_now());
    let vm = TuiRenderUnit::TuiAssistantBubble(value);
    let context = context(EntrySurface::Detail);
    cache.ensure(&vm, &GridSpec::with_content(60), context.clone(), 0);
    let parsed = cache.markdown.stable_parsed_blocks();
    assert!(parsed > 0);
    let start = cache.markdown_lines.stable_start.unwrap();
    assert_eq!(
        cache.line(start),
        cache.markdown_lines.stable[0].lines.first()
    );
    assert!(cache.line(start).is_some_and(|line| !line.spans.is_empty()));
    assert_eq!(
        cache.ensure(&vm, &GridSpec::with_content(30), context, 0),
        EntryInvalidation::Layout
    );
    assert_eq!(cache.markdown.stable_parsed_blocks(), parsed);
}

#[test]
#[serial_test::serial]
fn eviction_releases_derived_ownership_and_rehydrates_original_content() {
    let mut cache = EntryRenderCache::default();
    let vm = bubble(&"long body ".repeat(100));
    let grid = GridSpec::with_content(60);
    let context = context(EntrySurface::Message);
    cache.ensure(&vm, &grid, context.clone(), 0);
    let expected = cache.lines().as_ref().clone();
    assert!(cache.retained_bytes() > 0);
    cache.evict();
    assert_eq!(cache.retained_bytes(), 0);
    assert!(cache.lines().is_empty());
    assert!(cache.markdown_lines().stable.is_empty());
    assert!(cache.copy_button().is_none());
    assert_eq!(
        cache.ensure(&vm, &grid, context, 0),
        EntryInvalidation::Content
    );
    assert_eq!(cache.lines().as_ref(), &expected);
}
