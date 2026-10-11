use super::*;
use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAssistantBubble, TuiReasoningBlock, TuiRenderUnit, TuiToolCard,
    TuiToolPresentation,
};

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

fn group_with_tool() -> TuiSubAgentGroup {
    let mut group = group("tool-run", "intro");
    let mut tool = TuiToolCard {
        tool_id: "tool-1".into(),
        tool_name: "Bash".into(),
        input_summary: "echo hello".into(),
        output_summary: "hello".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
        tool_calls_count: 0,
    };
    tool.recompute_hash();
    group
        .view_models
        .push_back(TuiRenderUnit::TuiToolCard(tool));
    group
}

#[test]
#[serial_test::serial]
fn detail_fold_is_local_and_survives_stream_update() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let grid = GridSpec::grid_for(60);
    let mut transcript = group_with_tool();
    cache.prepare(&transcript, &grid, theme.clone(), 0, Vec::new());
    let collapsed_height = cache.height();
    cache.focus_next(false);
    assert_eq!(cache.focused(), Some(1));
    assert!(cache.toggle_focused(false));
    cache.prepare(&transcript, &grid, theme.clone(), 0, Vec::new());
    assert!(cache.height() > collapsed_height);
    assert_eq!(
        fold_of(&transcript.view_models[1]),
        Some(FoldState::Collapsed)
    );
    transcript
        .view_models
        .push_back(group("tail", "streamed").view_models[0].clone());
    transcript
        .view_models
        .insert(1, group("inserted", "before tool").view_models[0].clone());
    cache.prepare(&transcript, &grid, theme, 0, Vec::new());
    assert_eq!(
        cache.folds.get(&FoldKey::Tool("tool-1".into())),
        Some(&FoldState::Expanded)
    );
    assert_eq!(cache.focused(), Some(2));
    assert!(cache.height() > collapsed_height);
}

#[test]
#[serial_test::serial]
fn detail_reasoning_expansion_remeasures_height() {
    let mut cache = DetailRenderCache::default();
    let mut transcript = group("reasoning", "");
    let mut bubble = TuiAssistantBubble {
        text: String::new(),
        reasoning: Some(TuiReasoningBlock {
            text: "first\nsecond\nthird".into(),
            fold: FoldState::Collapsed,
            status: EntryStatus::Completed,
            is_running: false,
            started_at: None,
            duration_ms: None,
        }),
        message_id: Some("message-1".into()),
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    transcript.view_models = im::vector![TuiRenderUnit::TuiAssistantBubble(bubble.into())];
    let grid = GridSpec::grid_for(60);
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    cache.prepare(&transcript, &grid, theme.clone(), 0, Vec::new());
    let collapsed = cache.height();
    cache.focus_next(false);
    assert!(cache.toggle_focused(false));
    cache.prepare(&transcript, &grid, theme, 0, Vec::new());
    assert!(cache.height() > collapsed);
    assert_eq!(
        fold_of(&transcript.view_models[0]),
        Some(FoldState::Collapsed)
    );
}

#[test]
#[serial_test::serial]
fn detail_click_toggles_only_foldable_entry_header() {
    let mut cache = DetailRenderCache::default();
    let transcript = group_with_tool();
    let grid = GridSpec::grid_for(30);
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    cache.prepare(
        &transcript,
        &grid,
        theme.clone(),
        0,
        vec![Line::from("header")],
    );
    assert!(!cache.click_fold_header(0));
    assert!(!cache.click_fold_header(1));
    let tool_row = cache.header.len() + cache.prefix[1];
    assert!(cache.click_fold_header(tool_row));
    cache.prepare(&transcript, &grid, theme, 0, vec![Line::from("header")]);
    assert_eq!(cache.focused(), Some(1));
    assert!(!cache.click_fold_header(tool_row + 1));
    assert_eq!(
        cache.folds.get(&FoldKey::Tool("tool-1".into())),
        Some(&FoldState::Expanded)
    );
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
