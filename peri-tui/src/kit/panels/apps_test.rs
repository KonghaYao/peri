use super::*;
use crate::kit::apps::state::{AppEntry, AppView, AppsState, ViewPhase};
use ratatui_kit::ratatui::style::Color;

fn color_theme() -> (Color, Color, Color, Color) {
    (Color::White, Color::Gray, Color::Red, Color::Cyan)
}

fn entry() -> AppEntry {
    AppEntry {
        session_id: "apps-panel-session".to_string(),
        app_id: "app-1".to_string(),
        server_id: "demo".to_string(),
        resource_uri: "ui://get-time/mcp-app.html".to_string(),
        instantiating_tool: "get-time".to_string(),
        tool_call_id: Some("apps-call-1".to_string()),
    }
}

fn view_with_frame() -> AppView {
    let mut view = AppView::new("v1", entry());
    view.phase = ViewPhase::Ready;
    view.frame = Some(test_frame(7));
    view.pending_requests = 1;
    view
}

#[test]
fn header_shows_name_phase_frame_and_index() {
    crate::i18n::init(Some("en"));
    let mut state = AppsState::default();
    let view = view_with_frame();
    state.views.push(view.clone());
    state.active_view = Some(view.view_id.clone());
    let (primary, secondary, error, _) = color_theme();
    let lines = header_lines(&state, &view, primary, secondary, error);
    let text = rendered_text(&lines);
    assert!(text.contains("get-time"), "{text}");
    assert!(text.contains("ready"), "{text}");
    assert!(text.contains("frame #7"), "{text}");
    assert!(text.contains("1280×720"), "{text}");
    assert!(text.contains("pending 1"), "{text}");
    assert!(text.contains("[1/1]"), "{text}");
}

#[test]
fn header_surfaces_error_code_only() {
    crate::i18n::init(Some("en"));
    let mut view = AppView::new("v1", entry());
    view.phase = ViewPhase::Failed {
        code: "navigation_failed".to_string(),
    };
    let mut state = AppsState::default();
    state.views.push(view.clone());
    state.active_view = Some(view.view_id.clone());
    let (primary, secondary, error, _) = color_theme();
    let text = rendered_text(&header_lines(&state, &view, primary, secondary, error));
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("navigation_failed"), "{text}");
}

#[test]
fn placeholder_without_graphics_still_reports_frame_metadata() {
    crate::i18n::init(Some("en"));
    let image = std::sync::Arc::new(image::DynamicImage::new_rgb8(4, 4));
    let snapshot = (7u64, image, 1280u32, 720u32);
    let caps = crate::kit::terminal_caps::TerminalCaps::default();
    let (_, secondary, error, _) = color_theme();
    let lines = placeholder_lines("v1", Some(&snapshot), false, false, &caps, secondary, error);
    let text = rendered_text(&lines);
    assert!(text.contains("frame #7"), "{text}");
    assert!(text.contains("no image protocol"), "{text}");
}

#[test]
fn placeholder_while_encoding_mentions_encoding() {
    crate::i18n::init(Some("en"));
    let image = std::sync::Arc::new(image::DynamicImage::new_rgb8(4, 4));
    let snapshot = (3u64, image, 640u32, 480u32);
    let caps = crate::kit::terminal_caps::TerminalCaps::default();
    let (_, secondary, error, _) = color_theme();
    let text = rendered_text(&placeholder_lines(
        "v1",
        Some(&snapshot),
        true,
        false,
        &caps,
        secondary,
        error,
    ));
    assert!(text.contains("Encoding the frame"), "{text}");
}

#[test]
fn empty_state_distinguishes_disabled_from_no_apps() {
    crate::i18n::init(Some("en"));
    let (primary, secondary, error, _) = color_theme();
    let disabled = rendered_text(&empty_state_lines(
        false,
        &AppsState::default(),
        primary,
        secondary,
        error,
    ));
    assert!(disabled.contains("MCP Apps is off"), "{disabled}");

    let active = rendered_text(&empty_state_lines(
        true,
        &AppsState::default(),
        primary,
        secondary,
        error,
    ));
    assert!(active.contains("No completed app"), "{active}");
}

#[test]
fn descriptor_list_marks_selection() {
    crate::i18n::init(Some("en"));
    let mut state = AppsState::default();
    state.descriptors.push(entry());
    let (primary, secondary, _, accent) = color_theme();
    let text = rendered_text(&descriptor_lines(&state, 0, primary, secondary, accent));
    assert!(text.contains("Apps to open (1)"), "{text}");
    assert!(text.contains("▶ get-time"), "{text}");
}

fn card(tool_id: &str, is_running: bool) -> crate::kit::tui_render_unit::TuiToolCard {
    crate::kit::tui_render_unit::TuiToolCard {
        tool_id: tool_id.to_string(),
        tool_name: "get-time".to_string(),
        input_summary: String::new(),
        output_summary: String::new(),
        is_error: false,
        is_running,
        running_duration_ms: None,
        completed_duration_ms: Some(20),
        diff: None,
        presentation: crate::kit::tui_render_unit::TuiToolPresentation::default(),
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
        tool_calls_count: 0,
    }
}

fn collapsed_group(
    members: Vec<crate::kit::tui_render_unit::TuiRenderUnit>,
) -> crate::kit::tui_render_unit::TuiCollapsedGroup {
    let mut group = crate::kit::tui_render_unit::TuiCollapsedGroup {
        key: "tool:call-1".to_string(),
        title: "get-time".to_string(),
        count: u32::try_from(members.len()).unwrap_or(1),
        failed_count: 0,
        view_models: members,
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        is_working: false,
        content_hash: 0,
    };
    group.recompute_hash();
    group
}

#[serial_test::serial]
#[test]
fn collapsed_group_with_single_app_member_shows_action_on_header() {
    crate::i18n::init(Some("en"));
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set("apps-panel-session".to_string());
    crate::kit::apps::state::reset_for_test();
    crate::kit::apps::state::publish_descriptors(vec![entry()]);
    let group = collapsed_group(vec![
        crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card("apps-call-1", false)),
    ]);
    assert!(crate::kit::apps::app_for_collapsed_group(&group).is_some());
    let vm = crate::kit::tui_render_unit::TuiRenderUnit::TuiCollapsedGroup(group);
    let grid = crate::kit::message_area::grid::GridSpec::grid_for(100);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, ..) =
        crate::kit::message_area::render::vm_to_lines_cached(&vm, &grid, &mut cache, true);
    let text = rendered_text(&lines);
    assert!(text.contains("Open app · get-time"), "{text}");

    crate::kit::apps::state::reset_for_test();
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set(String::new());
}

#[serial_test::serial]
#[test]
fn group_with_two_app_members_has_no_header_action() {
    crate::i18n::init(Some("en"));
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set("apps-panel-session".to_string());
    crate::kit::apps::state::reset_for_test();
    let mut second = entry();
    second.app_id = "app-2".to_string();
    second.tool_call_id = Some("apps-call-2".to_string());
    crate::kit::apps::state::publish_descriptors(vec![entry(), second]);
    let group = collapsed_group(vec![
        crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card("apps-call-1", false)),
        crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card("apps-call-2", false)),
    ]);
    assert!(
        crate::kit::apps::app_for_collapsed_group(&group).is_none(),
        "多 App 组头不得暗示只打开其中一个"
    );
    crate::kit::apps::state::reset_for_test();
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set(String::new());
}

#[serial_test::serial]
#[test]
fn completed_tool_card_renders_open_app_action() {
    crate::i18n::init(Some("en"));
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set("apps-panel-session".to_string());
    crate::kit::apps::state::reset_for_test();
    crate::kit::apps::state::publish_descriptors(vec![entry()]);

    let vm = crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card("apps-call-1", false));
    let grid = crate::kit::message_area::grid::GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, ..) =
        crate::kit::message_area::render::vm_to_lines_cached(&vm, &grid, &mut cache, true);
    let text = rendered_text(&lines);
    assert!(text.contains("Open app · get-time"), "{text}");

    crate::kit::apps::state::reset_for_test();
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set(String::new());
}

#[serial_test::serial]
#[test]
fn running_tool_card_has_no_open_app_action() {
    crate::i18n::init(Some("en"));
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set("apps-panel-session".to_string());
    crate::kit::apps::state::reset_for_test();
    crate::kit::apps::state::publish_descriptors(vec![entry()]);

    let vm = crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card("apps-call-1", true));
    let grid = crate::kit::message_area::grid::GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, ..) =
        crate::kit::message_area::render::vm_to_lines_cached(&vm, &grid, &mut cache, true);
    let text = rendered_text(&lines);
    assert!(!text.contains("Open app"), "{text}");

    crate::kit::apps::state::reset_for_test();
    crate::kit::atoms::ACTIVE_SESSION_ID
        .state()
        .set(String::new());
}

#[test]
fn click_target_maps_inside_frame_and_rejects_letterbox() {
    crate::i18n::init(Some("en"));
    let mut view = view_with_frame();
    view.view_session_id = Some("view-1".to_string());
    let area = ratatui_kit::ratatui::layout::Rect::new(0, 0, 40, 20);
    let cell = crate::kit::apps::mapping::CellSize {
        width: 8,
        height: 16,
    };

    // 16:9 帧在 40x20 单元格中占 40x11，垂直居中（y=4）：
    // 点击画布内 → 映射到视口；点击上下黑边 → None（零请求）。
    let inside = click_target(&view, area, cell, 20, 8).expect("inside frame");
    assert_eq!(inside.0, "v1");
    assert!((inside.1.x - 20.5 / 40.0 * 1280.0).abs() < 0.01);
    assert!((inside.1.y - 4.5 / 11.0 * 720.0).abs() < 0.01);

    assert!(
        click_target(&view, area, cell, 20, 0).is_none(),
        "上方黑边不得发送点击"
    );
    assert!(
        click_target(&view, area, cell, 20, 19).is_none(),
        "下方黑边不得发送点击"
    );
    assert!(click_target(&view, area, cell, 41, 8).is_none());
    assert!(click_target(&view, area, cell, 20, 21).is_none());
}

#[test]
fn click_target_requires_frame_and_gateway_session() {
    crate::i18n::init(Some("en"));
    let area = ratatui_kit::ratatui::layout::Rect::new(0, 0, 40, 20);
    let cell = crate::kit::apps::mapping::CellSize {
        width: 8,
        height: 16,
    };

    let mut no_frame = AppView::new("v1", entry());
    no_frame.view_session_id = Some("view-1".to_string());
    assert!(click_target(&no_frame, area, cell, 20, 8).is_none());

    let mut no_session = view_with_frame();
    no_session.view_session_id = None;
    assert!(click_target(&no_session, area, cell, 20, 8).is_none());
}

// ── 面板内列表模式（A6：同时打开两个视图的入口）─────────────────────────────

#[test]
fn list_entry_key_toggles_only_when_views_exist() {
    let none = KeyModifiers::NONE;
    assert_eq!(
        apps_key_action(KeyCode::Char('o'), none, true, false, 1),
        AppsKeyAction::ToggleList,
        "有视图时 o 进入列表"
    );
    assert_eq!(
        apps_key_action(KeyCode::Char('o'), none, true, true, 1),
        AppsKeyAction::ToggleList,
        "列表模式下 o 退出列表"
    );
    assert_eq!(
        apps_key_action(KeyCode::Char('o'), none, false, false, 1),
        AppsKeyAction::Ignored,
        "无视图时 o 不响应（descriptor 列表已是默认界面）"
    );
}

#[test]
fn list_mode_routes_arrows_and_enter_to_descriptors() {
    let none = KeyModifiers::NONE;
    assert_eq!(
        apps_key_action(KeyCode::Up, none, true, true, 2),
        AppsKeyAction::SelectPrev
    );
    assert_eq!(
        apps_key_action(KeyCode::Down, none, true, true, 2),
        AppsKeyAction::SelectNext
    );
    assert_eq!(
        apps_key_action(KeyCode::Enter, none, true, true, 2),
        AppsKeyAction::OpenSelected
    );
    assert_eq!(
        apps_key_action(KeyCode::Down, none, true, false, 2),
        AppsKeyAction::Ignored,
        "非列表模式下方向键不移动被隐藏列表的选中项"
    );
    assert_eq!(
        apps_key_action(KeyCode::Enter, none, true, false, 2),
        AppsKeyAction::Ignored
    );
}

#[test]
fn list_mode_swallows_view_actions_but_not_esc() {
    let none = KeyModifiers::NONE;
    for code in [
        KeyCode::Char('x'),
        KeyCode::Char('r'),
        KeyCode::Tab,
        KeyCode::BackTab,
    ] {
        assert_eq!(
            apps_key_action(code, none, true, true, 2),
            AppsKeyAction::Swallow,
            "列表模式的 {code:?} 必须消费但不改变视图（BackTab 不得漏到全局）"
        );
    }
    assert_eq!(
        apps_key_action(KeyCode::Esc, none, true, true, 2),
        AppsKeyAction::Ignored,
        "Esc 必须交全局 handler 关闭面板"
    );
}

#[test]
fn view_mode_keeps_existing_shortcuts() {
    let none = KeyModifiers::NONE;
    assert_eq!(
        apps_key_action(KeyCode::Tab, none, true, false, 2),
        AppsKeyAction::CycleView(1)
    );
    assert_eq!(
        apps_key_action(KeyCode::BackTab, none, true, false, 2),
        AppsKeyAction::CycleView(-1)
    );
    assert_eq!(
        apps_key_action(KeyCode::Char('x'), none, true, false, 2),
        AppsKeyAction::CloseCurrent
    );
    assert_eq!(
        apps_key_action(KeyCode::Char('r'), none, true, false, 2),
        AppsKeyAction::ReopenCurrent
    );
}

#[test]
fn enter_resolves_selected_descriptor_app_id() {
    let mut state = AppsState::default();
    state.descriptors.push(entry());
    let mut second = entry();
    second.app_id = "app-2".to_string();
    state.descriptors.push(second);

    assert_eq!(selected_app_id(&state, 0).as_deref(), Some("app-1"));
    assert_eq!(selected_app_id(&state, 1).as_deref(), Some("app-2"));
    assert_eq!(
        selected_app_id(&state, 9).as_deref(),
        Some("app-2"),
        "descriptor 收缩后按最后一项收敛"
    );
    assert_eq!(selected_app_id(&AppsState::default(), 0), None);
}

#[test]
fn descriptor_list_marks_moved_selection() {
    crate::i18n::init(Some("en"));
    let mut state = AppsState::default();
    state.descriptors.push(entry());
    let mut second = entry();
    second.app_id = "app-2".to_string();
    second.instantiating_tool = "get-weather".to_string();
    state.descriptors.push(second);
    let (primary, secondary, _, accent) = color_theme();
    let text = rendered_text(&descriptor_lines(&state, 1, primary, secondary, accent));
    assert!(text.contains("  get-time"), "{text}");
    assert!(text.contains("▶ get-weather"), "{text}");
}

#[test]
fn apps_footer_hints_localized_in_both_locales() {
    for lang in ["en", "zh-CN"] {
        let lc = crate::i18n::LcRegistry::new(Some(lang));
        let args = vec![("index".to_string(), "1/2".into())];
        let footer = lc.tr_args("apps-footer-hint", &args);
        assert_ne!(footer, "apps-footer-hint", "{lang}: 缺少 apps-footer-hint");
        assert!(
            footer.contains("o "),
            "{lang}: 页脚未提示 o 入口（{footer}）"
        );

        let list_footer = lc.tr("apps-list-footer-hint");
        assert_ne!(
            list_footer, "apps-list-footer-hint",
            "{lang}: 缺少 apps-list-footer-hint"
        );
        assert!(
            list_footer.contains("Enter") && list_footer.contains('o'),
            "{lang}: 列表页脚缺少 Enter/o 提示（{list_footer}）"
        );
    }
}
