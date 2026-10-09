use super::*;
use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};
use crate::kit::tui_render_unit::{FoldState, TuiAssistantBubble, TuiRenderUnit};

fn group(instance_id: &str, text: &str) -> TuiSubAgentGroup {
    let mut bubble = TuiAssistantBubble {
        text: text.into(),
        reasoning: None,
        message_id: None,
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    TuiSubAgentGroup {
        instance_id: instance_id.into(),
        agent_id: "agent".into(),
        agent_name: "coder".into(),
        view_models: im::vector![TuiRenderUnit::TuiAssistantBubble(bubble.into())],
        collapsed: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    }
}

fn render(
    cache: &mut DetailRenderCache,
    group: &TuiSubAgentGroup,
    grid: &GridSpec,
    theme: Arc<ThemeDefinition>,
    language: u64,
) -> Vec<Line<'static>> {
    cache.prepare(group, grid, theme, language, Vec::new());
    cache.visible_lines(0..cache.height())
}

#[test]
#[serial_test::serial]
fn unchanged_detail_reuses_parse_and_context_changes_invalidate() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let group = group("first", "# cached text");
    let grid = GridSpec::with_content(60);
    let first = render(&mut cache, &group, &grid, theme.clone(), 0);
    reset_perf_counters();
    let second = render(&mut cache, &group, &grid, theme.clone(), 0);
    assert_eq!(first, second);
    assert_eq!(perf_counters().full_parses, 0);
    render(
        &mut cache,
        &group,
        &GridSpec::with_content(30),
        theme.clone(),
        0,
    );
    assert!(perf_counters().full_parses > 0);
    reset_perf_counters();
    render(&mut cache, &group, &grid, theme.clone(), 1);
    assert!(perf_counters().full_parses > 0);
    reset_perf_counters();
    render(
        &mut cache,
        &group,
        &grid,
        Arc::new(peri_theme::builtin::light_theme()),
        1,
    );
    assert!(perf_counters().full_parses > 0);
}

#[test]
#[serial_test::serial]
fn selected_occurrence_cannot_reuse_previous_slots() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let grid = GridSpec::with_content(60);
    render(
        &mut cache,
        &group("first", "old text"),
        &grid,
        theme.clone(),
        0,
    );
    reset_perf_counters();
    let result = render(&mut cache, &group("second", "new text"), &grid, theme, 0);
    assert!(perf_counters().full_parses > 0);
    let text: String = result
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("new text"));
    assert!(!text.contains("old text"));
}

#[test]
#[serial_test::serial]
fn visible_range_preserves_full_height_and_excludes_offscreen_history() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let mut transcript = group("range", "first");
    for index in 1..100 {
        transcript
            .view_models
            .push_back(group("nested", &format!("entry-{index}")).view_models[0].clone());
    }
    cache.prepare(
        &transcript,
        &GridSpec::with_content(60),
        theme.clone(),
        0,
        Vec::new(),
    );
    let total = cache.height();
    let rows = cache.prefix[50]..cache.prefix[51];
    reset_perf_counters();
    cache.prepare(
        &transcript,
        &GridSpec::with_content(60),
        theme,
        0,
        Vec::new(),
    );
    let visible = cache.visible_lines(rows);
    let text: String = visible
        .iter()
        .flat_map(|line| &line.spans)
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("entry-50"));
    assert!(!text.contains("entry-49"));
    assert!(!text.contains("entry-51"));
    assert_eq!(cache.height(), total);
    assert_eq!(perf_counters().full_parses, 0);
    assert_eq!(perf_counters().wrap_recalculated_lines, 0);
}

#[test]
#[serial_test::serial]
fn cold_slot_rehydrates_without_changing_height_or_copy_surface() {
    let mut cache = DetailRenderCache::default();
    let transcript = group("evict", &"long markdown text ".repeat(40));
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let grid = GridSpec::with_content(60);
    let expected = render(&mut cache, &transcript, &grid, theme, 0);
    let height = cache.height();
    let bytes = cache.slots[0].bytes();
    cache.slots[0].evict();
    cache.resident_slots.remove(&0);
    cache.resident_bytes = cache.resident_bytes.saturating_sub(bytes);
    assert!(cache.slots[0].wrap_map.is_empty());
    assert!(cache.slots[0].entry.lines().is_empty());
    let actual = cache.visible_lines(0..height);
    assert_eq!(actual, expected);
    assert_eq!(cache.height(), height);
    assert!(cache.slots[0].entry.copy_button().is_none());
}

#[test]
#[serial_test::serial]
fn same_length_replacement_and_empty_slots_update_prefix() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let grid = GridSpec::with_content(60);
    let first = group("same", "first");
    render(&mut cache, &first, &grid, theme.clone(), 0);
    let replacement = group("same", "replacement");
    let output = render(&mut cache, &replacement, &grid, theme.clone(), 0);
    assert!(
        output
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.content.contains("replacement"))
    );
    let mut empty = group("same", "");
    empty
        .view_models
        .push_back(group("same", "tail").view_models[0].clone());
    render(&mut cache, &empty, &grid, theme, 0);
    assert_eq!(cache.prefix[1], 0);
    assert_eq!(cache.slots_for(0..cache.height()), 1..2);
}

#[test]
#[serial_test::serial]
fn detail_draw_shows_nested_record_in_terminal_buffer() {
    let mut cache = DetailRenderCache::default();
    cache.prepare(
        &group("visible", "nested record"),
        &GridSpec::with_content(58),
        Arc::new(peri_theme::builtin::dark_theme()),
        0,
        vec![Line::from("coder"), Line::from("")],
    );
    let mut terminal = ratatui_kit::ratatui::Terminal::new(
        ratatui_kit::ratatui::backend::TestBackend::new(60, 12),
    )
    .unwrap();
    let frame = terminal
        .draw(|frame| {
            let mut drawer = ComponentDrawer::new(frame, Rect::new(0, 0, 60, 12));
            cache.draw(&mut drawer, Rect::new(0, 1, 59, 10), 0);
        })
        .unwrap();
    let rendered: String = frame
        .buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("nested record"), "{rendered}");
}
