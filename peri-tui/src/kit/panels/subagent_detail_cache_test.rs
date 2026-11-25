use super::Arc;
use super::ComponentDrawer;
use super::DetailRenderCache;
use super::FoldKey;
use super::GridSpec;
use super::Line;
use super::Rect;
use super::ThemeDefinition;
use super::TuiSubAgentGroup;

use crate::kit::atoms::{ACTIVE_PANEL, BRIDGE_RESET_COUNTER, INPUT_RESTORE_TEXT};
use crate::kit::mermaid::MERMAID_DETAIL;
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
        preview_omitted: false,
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
fn replacing_evicted_preview_releases_body_and_obsolete_fold_state() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let grid = GridSpec::grid_for(60);
    let mut transcript = group_with_tool();
    let weak = match &transcript.view_models[0] {
        TuiRenderUnit::TuiAssistantBubble(body) => Arc::downgrade(body),
        _ => unreachable!(),
    };
    cache.prepare(&transcript, &grid, theme.clone(), 0, Vec::new());
    cache.focus_next(false);
    cache.toggle_focused(false);
    assert!(!cache.folds.is_empty());
    transcript.view_models = im::Vector::new();
    transcript.preview_omitted = true;
    cache.prepare(&transcript, &grid, theme, 0, Vec::new());
    assert!(
        weak.upgrade().is_none(),
        "detail cache kept an evicted preview alive"
    );
    assert!(cache.folds.is_empty());
    assert_eq!(cache.slots.capacity(), 0);
    assert_eq!(cache.source.len(), 0);
}

#[test]
#[serial_test::serial]
fn entering_history_releases_the_previous_preview_source() {
    let mut cache = DetailRenderCache::default();
    let transcript = group("old-instance", "large old preview");
    let weak = match &transcript.view_models[0] {
        TuiRenderUnit::TuiAssistantBubble(body) => Arc::downgrade(body),
        _ => unreachable!(),
    };
    cache.prepare(
        &transcript,
        &GridSpec::grid_for(60),
        Arc::new(peri_theme::builtin::dark_theme()),
        0,
        Vec::new(),
    );
    drop(transcript);
    assert!(weak.upgrade().is_some());
    cache.prepare_text(
        vec![Line::from("saved child history")],
        "saved body".into(),
        60,
    );
    assert!(weak.upgrade().is_none());
    assert_eq!(cache.source.len(), 0);
    assert_eq!(cache.height(), 2);
}

#[test]
#[serial_test::serial]
fn forwarded_reminder_can_expand_inside_child_detail_and_expire_its_fold() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let grid = GridSpec::grid_for(60);
    let mut transcript = group("reminder-instance", "");
    let mut reminder = crate::kit::tui_render_unit::TuiSystemReminder::legacy(
        "child reminder body\nsecond line".into(),
    );
    reminder.summary = "child reminder".into();
    reminder.recompute_hash();
    let key = FoldKey::SystemReminder(reminder.reminder_id);
    transcript.view_models = im::vector![TuiRenderUnit::TuiSystemReminder(reminder)];
    cache.prepare(&transcript, &grid, theme.clone(), 0, Vec::new());
    cache.focus_next(false);
    assert!(cache.toggle_focused(false));
    assert_eq!(cache.folds.get(&key), Some(&FoldState::Expanded));
    let expanded = super::projected(&transcript.view_models[0], cache.folds.get(&key).copied());
    assert!(
        matches!(expanded.as_ref(), TuiRenderUnit::TuiSystemReminder(reminder)
        if reminder.fold == FoldState::Expanded && reminder.body.contains("second line"))
    );
    drop(expanded);
    transcript.view_models = im::Vector::new();
    cache.prepare(&transcript, &grid, theme, 0, Vec::new());
    assert!(cache.folds.is_empty());
    assert!(cache.focused.is_none());
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
        crate::kit::tui_render_unit::fold_state_of(&transcript.view_models[1]),
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
        crate::kit::tui_render_unit::fold_state_of(&transcript.view_models[0]),
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
    crate::kit::acp_bridge::reset_perf_counters();
    let second = render(&mut cache, &group, &grid, theme.clone(), 0);
    assert_eq!(first, second);
    assert_eq!(crate::kit::acp_bridge::perf_counters().full_parses, 0);
    render(
        &mut cache,
        &group,
        &GridSpec::with_content(30),
        theme.clone(),
        0,
    );
    assert!(crate::kit::acp_bridge::perf_counters().full_parses > 0);
    crate::kit::acp_bridge::reset_perf_counters();
    render(&mut cache, &group, &grid, theme.clone(), 1);
    assert!(crate::kit::acp_bridge::perf_counters().full_parses > 0);
    crate::kit::acp_bridge::reset_perf_counters();
    render(
        &mut cache,
        &group,
        &grid,
        Arc::new(peri_theme::builtin::light_theme()),
        1,
    );
    assert!(crate::kit::acp_bridge::perf_counters().full_parses > 0);
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
    crate::kit::acp_bridge::reset_perf_counters();
    let result = render(&mut cache, &group("second", "new text"), &grid, theme, 0);
    assert!(crate::kit::acp_bridge::perf_counters().full_parses > 0);
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
    crate::kit::acp_bridge::reset_perf_counters();
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
    assert_eq!(crate::kit::acp_bridge::perf_counters().full_parses, 0);
    assert_eq!(
        crate::kit::acp_bridge::perf_counters().wrap_recalculated_lines,
        0
    );
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

/// 面板本地 fold 与主区共用 `FoldKey::of` 键控：SubAgent / Interaction 条目
/// 也可展开/收起；面板不新增嵌套打开（Q12 未批准），只切本地 fold。
#[test]
#[serial_test::serial]
fn panel_folds_subagent_and_interaction_with_shared_keys() {
    use crate::kit::tui_render_unit::{FoldKey, InteractionKind, TuiAskUserBlock};

    let mut transcript = group("panel-keys", "");
    transcript.view_models = im::vector![
        TuiRenderUnit::TuiSubAgentGroup(group("nested-1", "child body")),
        TuiRenderUnit::TuiAskUserBlock(TuiAskUserBlock {
            items: vec![],
            is_error: false,
            kind: InteractionKind::AskUser,
            pending: false,
            verb: "AskUser".into(),
            question: "Which one?".into(),
            options: vec!["A".into()],
            result: Some("A".into()),
            request_id: Some("rid-1".into()),
            owner: None,
            question_ids: vec![],
            fold: FoldState::Collapsed,
            user_modified: false,
            content_hash: 0,
        }),
    ];
    let grid = GridSpec::with_content(60);
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let mut cache = DetailRenderCache::default();
    cache.prepare(&transcript, &grid, theme, 0, Vec::new());

    let subagent_key = FoldKey::of(&transcript.view_models[0]).expect("subagent 条目应可折叠");
    let interaction_key =
        FoldKey::of(&transcript.view_models[1]).expect("interaction 条目应可折叠");
    assert_eq!(subagent_key, FoldKey::SubAgent("nested-1".into()));
    assert_eq!(interaction_key, FoldKey::Interaction("rid-1".into()));

    // 面板 Enter：只切换本地 fold，不写全局选择、不打开嵌套面板。
    let original_selection = crate::kit::atoms::SELECTED_SUBAGENT_ID
        .state()
        .read()
        .clone();
    let previous_selection = Some("outer-agent".to_owned());
    *crate::kit::atoms::SELECTED_SUBAGENT_ID.state().write() = previous_selection.clone();
    assert_eq!(
        cache.focus_next(false),
        Some(cache.header.len() + cache.prefix[0])
    );
    assert!(cache.toggle_focused(false));
    assert_eq!(cache.folds.get(&subagent_key), Some(&FoldState::Expanded));
    assert!(cache.focus_next(false).is_some());
    assert!(cache.toggle_focused(false));
    assert_eq!(
        cache.folds.get(&interaction_key),
        Some(&FoldState::Expanded)
    );
    assert_eq!(
        *crate::kit::atoms::SELECTED_SUBAGENT_ID.state().read(),
        previous_selection,
        "面板 fold 不得触发嵌套打开"
    );
    *crate::kit::atoms::SELECTED_SUBAGENT_ID.state().write() = original_selection;
}

/// 面板关闭（`clear`）与 subagent 身份变化必须清空本地 fold——canonical 内容
/// 只读，临时展开不跨面板实例泄漏。
#[test]
#[serial_test::serial]
fn panel_close_and_instance_switch_clear_local_folds() {
    let mut cache = DetailRenderCache::default();
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let grid = GridSpec::with_content(60);
    let transcript = group_with_tool();

    cache.prepare(&transcript, &grid, theme.clone(), 0, Vec::new());
    cache.focus_next(false);
    assert!(cache.toggle_focused(false));
    let key = FoldKey::Tool("tool-1".into());
    assert_eq!(cache.folds.get(&key), Some(&FoldState::Expanded));

    // 身份变化：切到另一个 subagent → 本地 fold 清空。
    cache.prepare(
        &group("other", "other body"),
        &grid,
        theme.clone(),
        0,
        Vec::new(),
    );
    assert!(cache.folds.is_empty(), "身份变化必须清空本地 fold");
    assert!(cache.focused.is_none(), "身份变化必须清空焦点");

    // 重新打开并展开，再关闭面板（clear）→ 本地 fold 清空。
    cache.prepare(&transcript, &grid, theme, 0, Vec::new());
    cache.focus_next(false);
    assert!(cache.toggle_focused(false));
    assert!(!cache.folds.is_empty());
    cache.clear(Vec::new());
    assert!(cache.folds.is_empty(), "面板关闭必须清空本地 fold");
    assert!(cache.focused.is_none());
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
    .expect("detail draw shows nested record in terminal buffer: fixture parameters must construct a valid value");
    let frame = terminal
        .draw(|frame| {
            let mut drawer = ComponentDrawer::new(frame, Rect::new(0, 0, 60, 12));
            cache.draw(&mut drawer, Rect::new(0, 1, 59, 10), 0);
        })
        .expect("detail draw shows nested record in terminal buffer: fixture parameters must construct a valid value");
    let rendered: String = frame
        .buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("nested record"), "{rendered}");
}

// ── 面板副产物命中：GenUI 按钮 / Mermaid「查看图」 ────────────────────────

const ARTIFACT_TEXT: &str = r#"intro

```gen-ui
$count = 1
root = Stack([Text("count=" + $count), Button("add", @Set($count, $count + 1))])
```

```mermaid
graph TD
A-->B
```"#;

/// 动作测试的全局状态隔离：会话 epoch、输入恢复文本、面板栈与图详情。
struct ArtifactGuard {
    restore: Option<String>,
    epoch: u64,
    active_panel: Option<crate::app::panel_types::PanelKind>,
    mermaid: Option<crate::kit::mermaid::MermaidDetail>,
}

impl ArtifactGuard {
    fn new() -> Self {
        crate::kit::atoms::init_atoms();
        let guard = Self {
            restore: INPUT_RESTORE_TEXT
                .get_or_init(Default::default)
                .lock()
                .take(),
            epoch: BRIDGE_RESET_COUNTER.get(),
            active_panel: *ACTIVE_PANEL.state().read(),
            mermaid: MERMAID_DETAIL.state().read().clone(),
        };
        crate::kit::panel_registry::close_panel(crate::app::panel_types::PanelKind::MermaidDiagram);
        *MERMAID_DETAIL.state().write() = None;
        BRIDGE_RESET_COUNTER.set(guard.epoch.wrapping_add(1));
        guard
    }
}

impl Drop for ArtifactGuard {
    fn drop(&mut self) {
        crate::kit::panel_registry::close_panel(crate::app::panel_types::PanelKind::MermaidDiagram);
        *MERMAID_DETAIL.state().write() = self.mermaid.take();
        ACTIVE_PANEL.set(self.active_panel);
        *INPUT_RESTORE_TEXT.get_or_init(Default::default).lock() = self.restore.take();
        BRIDGE_RESET_COUNTER.set(self.epoch);
    }
}

fn artifact_cache(
    transcript: &TuiSubAgentGroup,
    grid: &GridSpec,
) -> (DetailRenderCache, Vec<Line<'static>>, Arc<ThemeDefinition>) {
    let theme = Arc::new(peri_theme::builtin::dark_theme());
    let header = vec![Line::from("header")];
    let mut cache = DetailRenderCache::default();
    cache.prepare(transcript, grid, theme.clone(), 0, header.clone());
    (cache, header, theme)
}

#[test]
#[serial_test::serial]
fn panel_artifacts_keep_genui_and_mermaid_hits_with_content_geometry() {
    let transcript = group("artifact-run", ARTIFACT_TEXT);
    let grid = GridSpec::grid_for(80);
    let (cache, _, _) = artifact_cache(&transcript, &grid);
    let area = Rect::new(4, 7, grid.line_width(), 30);
    let artifacts = super::collect_artifacts(&cache, area, 0);

    assert_eq!(artifacts.genui.len(), 1, "genui button must keep a hit");
    assert_eq!(artifacts.mermaid.len(), 1, "mermaid action must keep a hit");

    let button = &cache.slots[0].entry.genui_buttons()[0];
    let hit = &artifacts.genui[0];
    let expected_row = area.y as usize
        + cache.header.len()
        + cache.prefix[0]
        + cache.slots[0].wrap_map[button.line_start].visual_start;
    assert_eq!(hit.row as usize, expected_row, "content row → button row");
    assert_eq!(hit.x_start, area.x + button.x_start);
    assert_eq!(hit.x_end, area.x + button.x_end);

    let span = &cache.slots[0].entry.mermaid_fences()[0];
    let action_row = span.logical.end - 1;
    let expected_row = area.y as usize
        + cache.header.len()
        + cache.prefix[0]
        + cache.slots[0].wrap_map[action_row].visual_start;
    let mermaid = &artifacts.mermaid[0];
    assert_eq!(
        mermaid.row as usize, expected_row,
        "fence tail → action row"
    );
    assert_eq!(mermaid.x_start, area.x + grid.cont_prefix_width() as u16);
    assert_eq!(mermaid.x_end, area.x + area.width - 1);

    // 滚动偏移与 draw 同口径：命中行随 top 上移。
    let scrolled = super::collect_artifacts(&cache, area, 2);
    assert_eq!(scrolled.genui[0].row + 2, hit.row);
    assert_eq!(scrolled.mermaid[0].row + 2, mermaid.row);
}

#[test]
#[serial_test::serial]
fn panel_artifact_clicks_reuse_main_genui_and_mermaid_actions() {
    let _guard = ArtifactGuard::new();
    let transcript = group("artifact-click", ARTIFACT_TEXT);
    let grid = GridSpec::grid_for(80);
    let (mut cache, header, theme) = artifact_cache(&transcript, &grid);
    let area = Rect::new(0, 0, grid.line_width(), 40);
    let artifacts = super::collect_artifacts(&cache, area, 0);

    let button = cache.slots[0].entry.genui_buttons()[0].clone();
    let block = button.block.expect("complete gen-ui block");
    let before = crate::kit::genui::view_state::block_revision(block);
    let hit = artifacts
        .genui
        .iter()
        .find(|hit| hit.button == button)
        .expect("button hit");
    assert!(super::apply_artifact_click(super::resolve_artifact_click(
        &cache,
        &artifacts,
        hit.row,
        hit.x_start
    )));
    assert!(
        crate::kit::genui::view_state::block_revision(block) > before,
        "panel click must execute the same Set action"
    );

    // Set 改变按钮/正文后，prepare 必须重算行高并刷新渲染，而不是停在旧帧。
    let prepared = cache.prepare(&transcript, &grid, theme, 0, header);
    assert!(!prepared, "same instance: no scroll reset");
    let text: String = (0..cache.slots[0].height)
        .filter_map(|row| cache.slots[0].entry.line(row))
        .map(|line| line.to_string())
        .collect();
    assert!(
        text.contains("count=2"),
        "panel body must follow gen-ui state"
    );

    // Mermaid 动作行走主区同一打开入口；面板不携带主区 slot 身份。
    // Set 后 slot 已重建，先按新帧重建命中（真实路径由 post_draw 完成）。
    let artifacts = super::collect_artifacts(&cache, area, 0);
    let mermaid = artifacts.mermaid[0].clone();
    assert!(super::apply_artifact_click(super::resolve_artifact_click(
        &cache,
        &artifacts,
        mermaid.row,
        mermaid.x_start
    )));
    let detail = MERMAID_DETAIL
        .state()
        .read()
        .clone()
        .expect("mermaid detail opened from panel");
    assert!(detail.source.contains("A-->B"));
    assert_eq!(detail.slot, None);
    assert_eq!(detail.vm_hash, None);
}

#[test]
#[serial_test::serial]
fn panel_artifact_click_is_rejected_after_cache_rebuild() {
    let _guard = ArtifactGuard::new();
    let transcript = group("artifact-stale", ARTIFACT_TEXT);
    let grid = GridSpec::grid_for(80);
    let (mut cache, header, theme) = artifact_cache(&transcript, &grid);
    let area = Rect::new(0, 0, grid.line_width(), 40);
    let artifacts = super::collect_artifacts(&cache, area, 0);
    let hit = artifacts.genui[0].clone();
    let block = hit.button.block.expect("complete gen-ui block");
    let before = crate::kit::genui::view_state::block_revision(block);

    let mut rebuilt = group("artifact-stale", "intro only");
    rebuilt.instance_id = transcript.instance_id.clone();
    cache.prepare(&rebuilt, &grid, theme, 0, header);

    // 旧热区仍被认领（不让点击落到下层），但不执行动作。
    assert!(super::apply_artifact_click(super::resolve_artifact_click(
        &cache,
        &artifacts,
        hit.row,
        hit.x_start
    )));
    assert_eq!(
        crate::kit::genui::view_state::block_revision(block),
        before,
        "stale hit must not dispatch"
    );
}
