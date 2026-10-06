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

#[test]
#[serial_test::serial]
fn unchanged_detail_reuses_parse_and_context_changes_invalidate() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let group = group("first", "# cached text");
    let grid = GridSpec::with_content(60);
    let first = cache.render(&group, &grid, theme.clone(), 0);
    reset_perf_counters();
    let second = cache.render(&group, &grid, theme.clone(), 0);
    assert_eq!(first, second);
    assert_eq!(perf_counters().full_parses, 0);
    cache.render(&group, &GridSpec::with_content(30), theme.clone(), 0);
    assert!(perf_counters().full_parses > 0);
    reset_perf_counters();
    cache.render(&group, &grid, theme.clone(), 1);
    assert!(perf_counters().full_parses > 0);
    reset_perf_counters();
    cache.render(
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
    cache.render(&group("first", "old text"), &grid, theme.clone(), 0);
    reset_perf_counters();
    let result = cache.render(&group("second", "new text"), &grid, theme, 0);
    assert!(perf_counters().full_parses > 0);
    let text: String = result
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.contains("new text"));
    assert!(!text.contains("old text"));
}
