//! Tests
use super::BgDisplayEntry;
use super::GridSpec;
use super::Rect;
use super::TuiRenderUnit;

use peri_tui_components::scrollbar::{VerticalBar, VerticalPress};
use ratatui::backend::TestBackend;
use ratatui_kit::prelude::*;
use std::task::{Context, Poll, Waker};

use crate::kit::tui_render_unit::{TuiSubAgentGroup, TuiUserBubble};

#[must_use]
fn make_subagent(id: &str, name: &str) -> TuiSubAgentGroup {
    TuiSubAgentGroup {
        instance_id: format!("instance-{id}"),
        agent_id: id.to_string(),
        agent_name: name.to_string(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        preview_omitted: false,
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    }
}

/// Q11：身份头几何取 Q9 记录的同一份内容区——prepare 网格与绘制视口同源。
fn measured_detail(width: u16) -> (GridSpec, usize) {
    let mut geometry = super::DetailGeometry::default();
    geometry.observe(Rect::new(0, 2, width, 24));
    let content = geometry.content().expect("measured content area");
    let layout = geometry.prepare_layout().expect("measured prepare layout");
    (layout.grid, super::detail_viewport(content).width as usize)
}

fn lines_text(lines: &[ratatui_kit::ratatui::text::Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// running 符号帧判定：braille 动画帧或 ASCII 降级 `*`（与主区测试同口径）。
fn is_running_frame(symbol: &str) -> bool {
    symbol.chars().any(|c| {
        c == '*' || crate::components::spinner::animation::BRAILLE_LOADING_FRAMES.contains(&c)
    })
}

#[test]
fn test_subagent_detail_content_height_exposes_scroll_overflow() {
    let viewport_height = 20u16;
    let content_height = super::scroll_content_height(80);
    let bottom_offset = content_height.saturating_sub(viewport_height);

    assert_eq!(content_height, 80);
    assert!(content_height > viewport_height);
    assert_eq!(bottom_offset + viewport_height, content_height);
}

#[test]
fn test_subagent_detail_content_height_is_non_zero_and_saturating() {
    assert_eq!(super::scroll_content_height(0), 1);
    assert_eq!(super::scroll_content_height(usize::MAX), u16::MAX);
}

#[test]
fn virtual_detail_offset_preserves_full_height_and_clamps_after_resize() {
    assert_eq!(super::clamp_detail_offset(90_000, 100_000, 20), 90_000);
    assert_eq!(super::clamp_detail_offset(100_000, 100_000, 20), 99_980);
    assert_eq!(super::clamp_detail_offset(99_980, 100_000, 100), 99_900);
    assert_eq!(super::clamp_detail_offset(50, 10, 20), 0);
}

#[test]
fn detail_viewport_reserves_scrollbar_column_inside_content_area() {
    // 面板外框 → 内容区（扣上下边框/标题）→ 视口（再让出最右滚动条列）。
    let content = super::detail_content_area(Rect::new(4, 3, 60, 12));
    assert_eq!(content, Rect::new(4, 4, 60, 10));
    assert_eq!(super::detail_viewport(content), Rect::new(4, 4, 59, 10));
}

#[test]
fn detail_width_budget_matches_drawn_viewport() {
    for width in [2, 3, 6, 7, 30, 40, 60, 100, 120] {
        let content = super::detail_content_area(Rect::new(0, 0, width, 12));
        let viewport = super::detail_viewport(content);
        let mut grid = GridSpec::grid_for(width);
        grid.indent = 0;
        assert!(grid.line_width() <= viewport.width, "panel width {width}");
        if width >= 7 {
            assert!(
                grid.first_prefix_width() + grid.content_width() <= viewport.width as usize,
                "panel width {width}"
            );
        }
    }
}

/// Q11：身份头复用主时间线组头的状态符号映射（`status_symbol_and_color`），
/// running/completed/failed 三态符号与色彩一致；不新增第二套映射。
#[test]
fn detail_header_status_symbol_matches_shared_group_head_mapping() {
    let theme = peri_theme::builtin::dark_theme();
    let (grid, width) = measured_detail(80);
    for (running, error) in [(true, false), (false, false), (false, true)] {
        let mut group = make_subagent("alpha", "Alpha");
        group.is_running = running;
        group.is_error = error;
        let lines = super::detail_header_lines(&group, &grid, width, &theme);
        let text = lines_text(&lines);
        assert!(text.contains("Alpha"), "身份头含名称，实际 {text:?}");
        assert!(text.contains("[alpha]"), "身份头含 agent id，实际 {text:?}");

        let symbol_span = &lines[0].spans[0];
        let (expected_symbol, expected_color) =
            crate::kit::message_area::render::status_symbol_and_color(
                running,
                error,
                &theme.semantic,
            );
        assert_eq!(
            symbol_span.style.fg,
            Some(expected_color),
            "符号色取共享映射（running={running}, error={error}）"
        );
        if running {
            assert!(
                is_running_frame(&symbol_span.content),
                "running 使用动画符号，实际 {:?}",
                symbol_span.content
            );
        } else {
            assert_eq!(
                symbol_span.content.trim_end(),
                expected_symbol,
                "completed/failed 符号取共享映射（error={error}）"
            );
        }
    }
}

/// Q11：failed 且有原因时追加原因行；无原因不追加空行；completed 的 result
/// 文本（subagent_result 投影）不得显示为失败原因。
#[test]
fn detail_header_renders_failure_reason_only_for_failed_with_reason() {
    let theme = peri_theme::builtin::dark_theme();
    let (grid, width) = measured_detail(80);

    let mut failed = make_subagent("alpha", "Alpha");
    failed.is_error = true;
    failed.error_reason = Some("permission denied".into());
    let lines = super::detail_header_lines(&failed, &grid, width, &theme);
    assert!(
        lines.len() > 2,
        "failed 有原因须有原因行，实际 {} 行",
        lines.len()
    );
    assert!(
        lines_text(&lines).contains("permission denied"),
        "原因行显示 canonical error_reason，实际 {:?}",
        lines_text(&lines)
    );

    let mut bare = make_subagent("alpha", "Alpha");
    bare.is_error = true;
    let lines = super::detail_header_lines(&bare, &grid, width, &theme);
    assert_eq!(lines.len(), 2, "failed 无原因只保留身份两行");
    assert!(
        lines.iter().all(|line| !line.to_string().trim().is_empty()),
        "无原因不得渲染空行，实际 {lines:?}"
    );

    let mut completed = make_subagent("alpha", "Alpha");
    completed.error_reason = Some("done".into());
    let lines = super::detail_header_lines(&completed, &grid, width, &theme);
    assert_eq!(lines.len(), 2, "completed 不追加原因行");
    assert!(
        !lines_text(&lines).contains("done"),
        "成功 result 不作为失败原因显示，实际 {:?}",
        lines_text(&lines)
    );
}

/// Q11 数据源：bg live 明细的 `subagent_result` 经面板合成组进入身份头；
/// `subagent_is_error` 决定 failed 符号。
#[test]
fn detail_header_shows_live_subagent_failure_result_as_reason() {
    let theme = peri_theme::builtin::dark_theme();
    let (grid, width) = measured_detail(80);
    let mut detail = live_detail_for("bg-agent", "subagent-1", "partial");
    detail.subagent_result = Some("tool denied".into());
    detail.subagent_is_error = true;
    detail.status = crate::kit::atoms::BgLiveStatus::Failed;
    let live = std::collections::HashMap::from([("task-1".to_string(), detail)]);
    let group =
        super::find_live_detail_subagent(&live, &[], Some("task-1")).expect("live detail group");

    let lines = super::detail_header_lines(&group, &grid, width, &theme);
    assert!(
        lines_text(&lines).contains("tool denied"),
        "live detail 的 subagent_result 须出现在原因行，实际 {:?}",
        lines_text(&lines)
    );
    assert!(
        lines[0].spans[0].content.contains('\u{d7}'),
        "failed 状态符号须出现，实际 {:?}",
        lines[0].spans[0].content
    );
}

/// 首帧几何未知时不得按兜底宽度 prepare（旧路径 `Rect::default()` → 2 列）；
/// 记录内容区之后，prepare 用实测宽度派生网格与视口高度。
#[test]
fn detail_prepare_waits_for_measured_content_geometry() {
    let mut geometry = super::DetailGeometry::default();
    assert_eq!(geometry.content(), None);
    assert!(
        geometry.prepare_layout().is_none(),
        "几何未知时不应准备（否则会按极窄兜底宽度折行）"
    );

    geometry.observe(Rect::new(0, 2, 80, 24));
    assert_eq!(geometry.content(), Some(Rect::new(0, 3, 80, 22)));
    let layout = geometry
        .prepare_layout()
        .expect("记录内容区后 prepare 布局可用");
    let mut expected = GridSpec::grid_for(80);
    expected.indent = 0;
    assert_eq!(layout.grid, expected, "网格宽度取记录的内容区宽度");
    assert_eq!(layout.viewport_height, 22, "视口高度取内容区高度");
}

/// 首次测量与每次几何变化各请求一帧校正渲染（`MsgAreaTracker` 模式）：
/// 首帧 update 之后才记录到内容区，没有这次校正就不会按实测宽度重新 prepare。
#[test]
fn detail_geometry_requests_one_corrective_frame_per_geometry_change() {
    let mut geometry = super::DetailGeometry::default();
    let mut context = Context::from_waker(Waker::noop());
    assert_eq!(geometry.poll_change(&mut context), Poll::Pending);

    geometry.observe(Rect::new(0, 2, 80, 24));
    assert_eq!(geometry.poll_change(&mut context), Poll::Ready(()));
    assert_eq!(
        geometry.poll_change(&mut context),
        Poll::Pending,
        "同一几何不重复请求"
    );

    geometry.observe(Rect::new(0, 2, 80, 24));
    assert_eq!(geometry.poll_change(&mut context), Poll::Pending);

    geometry.observe(Rect::new(0, 2, 100, 24));
    assert_eq!(geometry.poll_change(&mut context), Poll::Ready(()));
}

/// draw 阶段从面板外框回填内容区：hook 的 `pre_component_draw` 看到的就是
/// 面板区域（`panel_shell!` 的边框在子节点绘制时才收窄 `drawer.area`）。
#[test]
fn detail_geometry_hook_records_draw_area() {
    let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 60))
        .expect("draw geometry: fixture parameters must construct a valid value");
    let mut geometry = super::DetailGeometry::default();
    let area = Rect::new(4, 2, 80, 24);
    terminal
        .draw(|frame| {
            let mut drawer = ComponentDrawer::new(frame, area);
            geometry.pre_component_draw(&mut drawer);
        })
        .expect("draw geometry: fixture parameters must construct a valid value");
    assert_eq!(geometry.content(), Some(Rect::new(4, 3, 80, 22)));
}

#[test]
fn growing_detail_follows_only_when_already_at_bottom() {
    assert_eq!(super::follow_detail_offset(20, 30, 10, 35, 10, true), 25);
    assert_eq!(super::follow_detail_offset(12, 30, 10, 35, 10, true), 12);
    assert_eq!(super::follow_detail_offset(20, 30, 10, 35, 10, false), 20);
    assert_eq!(super::follow_detail_offset(20, 30, 10, 15, 10, true), 5);
}

#[test]
fn running_detail_opens_at_live_tail_and_completed_detail_opens_at_header() {
    assert_eq!(super::initial_detail_offset(true, 30, 10), 20);
    assert_eq!(super::initial_detail_offset(false, 30, 10), 0);
}

#[test]
fn detail_scrollbar_drag_reaches_both_ends_without_thumb_jump() {
    // 详情视口高 10：上下箭头占 2 行，轨道 8 行；内容 1000 / 视口 10 → 底部 990。
    let area = Rect::new(59, 1, 1, 10);
    let top = VerticalBar::new(10, 10, 1000, 0).expect("overflowing detail");
    assert_eq!(top.max_offset(), 990);
    assert_eq!(top.press(area, area.y), VerticalPress::Top);
    assert_eq!(
        top.press(area, area.y + area.height - 2),
        VerticalPress::Track { offset: 990 }
    );
    assert_eq!(
        top.press(area, area.y + area.height - 1),
        VerticalPress::Bottom
    );

    let bottom = VerticalBar::new(10, 10, 1000, 990).expect("overflowing detail");
    assert_eq!(
        bottom.press(area, area.y + area.height - 2),
        VerticalPress::Thumb { grab: 0 }
    );
    assert_eq!(bottom.drag_offset(area, area.y + area.height - 2, 0), 990);
    let middle = VerticalBar::new(10, 10, 1000, 495).expect("overflowing detail");
    assert!(middle.thumb_start > 0);
    assert_eq!(
        middle.drag_offset(area, area.y + 1 + middle.thumb_start as u16, 0),
        495,
        "同一 thumb 格内拖拽保持原始偏移（495 反推是 566）"
    );
}

#[test]
fn test_find_selected_subagent_none_when_no_selection() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
            "alpha", "Alpha",
        ))]),
        generation: 0,
    };
    assert!(super::find_selected_subagent(&snap, None).is_none());
}

#[test]
fn test_find_selected_subagent_matches_by_id() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("alpha", "Alpha")),
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("beta", "Beta")),
        ]),
        generation: 0,
    };
    let g = super::find_selected_subagent(&snap, Some("beta")).expect("应匹配 beta");
    assert_eq!(g.agent_id, "beta");
    assert_eq!(g.agent_name, "Beta");
}

#[test]
fn test_find_selected_subagent_uses_instance_id_for_resumed_occurrence() {
    let mut first = make_subagent("shared", "First");
    first.instance_id = "occurrence-1".into();
    let mut resumed = make_subagent("shared", "Resumed");
    resumed.instance_id = "occurrence-2".into();
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiSubAgentGroup(first),
            TuiRenderUnit::TuiSubAgentGroup(resumed),
        ]),
        generation: 0,
    };

    let found =
        super::find_selected_subagent(&snap, Some("occurrence-2")).expect("resumed occurrence");
    assert_eq!(found.agent_name, "Resumed");
}

#[test]
fn test_find_selected_subagent_unknown_id_returns_none() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
            "alpha", "Alpha",
        ))]),
        generation: 0,
    };
    assert!(super::find_selected_subagent(&snap, Some("nope")).is_none());
}

#[test]
fn test_find_selected_subagent_recurses_into_nested_subagent() {
    // 嵌套 SubAgent 内层（罕见但支持，与 agent.rs collect_subagents 同口径）
    let mut outer = make_subagent("outer", "Outer");
    outer.view_models = im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
        "inner", "Inner",
    ))]);
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(outer)]),
        generation: 0,
    };
    let g = super::find_selected_subagent(&snap, Some("inner")).expect("应找到嵌套内层");
    assert_eq!(g.agent_id, "inner");
    let outer_match = super::find_selected_subagent(&snap, Some("outer")).expect("外层也可匹配");
    assert_eq!(outer_match.agent_id, "outer");
}

#[test]
fn test_find_live_detail_subagent_prefers_latest_resumed_task() {
    let older = crate::kit::atoms::BgLiveDetail {
        agent_id: Some("shared-agent".into()),
        agent_name: Some("Older".into()),
        ..Default::default()
    };
    let latest = crate::kit::atoms::BgLiveDetail {
        agent_id: Some("shared-agent".into()),
        agent_name: Some("Latest".into()),
        ..Default::default()
    };
    let live = std::collections::HashMap::from([
        ("task-z-old".to_string(), older),
        ("task-a-latest".to_string(), latest),
    ]);
    let now = std::time::Instant::now();
    let display = vec![
        crate::kit::atoms::BgDisplayEntry {
            id: "task-z-old".into(),
            linked_agent_id: Some("shared-agent".into()),
            agent_type: "agent".into(),
            desc: "Older".into(),
            current_tool: None,
            tool_count: 0,
            is_active: false,
            is_error: false,
            created_at: now,
            completed_at: Some(now),
        },
        crate::kit::atoms::BgDisplayEntry {
            id: "task-a-latest".into(),
            linked_agent_id: Some("shared-agent".into()),
            agent_type: "agent".into(),
            desc: "Latest".into(),
            current_tool: None,
            tool_count: 0,
            is_active: true,
            is_error: false,
            created_at: now,
            completed_at: None,
        },
    ];

    let found = super::find_live_detail_subagent(&live, &display, Some("shared-agent"))
        .expect("latest task");
    assert_eq!(found.instance_id, "task-a-latest");
    assert_eq!(found.agent_name, "Latest");
}

#[test]
fn test_find_selected_subagent_skips_non_subagent_vms() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "hi".to_string(),
            attachments: Vec::new(),
            content_hash: 0,
            reminder: None,
            source: None,
            message_id: None,
        })]),
        generation: 0,
    };
    assert!(super::find_selected_subagent(&snap, Some("alpha")).is_none());
}

// ── 后台 subagent 的权威源解析（回归：跨 turn 边界后组被冻结）─────────────

fn bg_display_entry(task_id: &str, agent_id: &str) -> BgDisplayEntry {
    BgDisplayEntry {
        id: task_id.into(),
        linked_agent_id: Some(agent_id.into()),
        agent_type: "agent".into(),
        desc: "bg".into(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: std::time::Instant::now(),
        completed_at: None,
    }
}

fn text_unit(text: &str) -> TuiRenderUnit {
    let mut bubble = crate::kit::tui_render_unit::TuiAssistantBubble {
        text: text.to_string(),
        reasoning: None,
        message_id: None,
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    TuiRenderUnit::TuiAssistantBubble(bubble.into())
}

/// 某次 bg 运行产生的 live 明细——记录它属于哪一次运行（`SubAgentAccumulator`
/// 的 instance_id）。
fn live_detail_for(
    agent_id: &str,
    instance_id: &str,
    text: &str,
) -> crate::kit::atoms::BgLiveDetail {
    crate::kit::atoms::BgLiveDetail {
        agent_id: Some(agent_id.into()),
        agent_name: Some("coder".into()),
        subagent_instance_id: Some(instance_id.into()),
        nested_units: im::Vector::from(vec![text_unit(text)]),
        ..Default::default()
    }
}

fn nested_texts(group: &TuiSubAgentGroup) -> Vec<String> {
    group
        .view_models
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(b) => Some(b.text.clone()),
            _ => None,
        })
        .collect()
}

/// bg subagent 的工具事件不进组、组在 turn 边界（TurnSuspended/TurnInterrupted）
/// 被归档冻结，此后内容只进 `BG_LIVE_DETAIL`。面板必须渲染 live detail——冻结的
/// 组即使先被扫描命中、且自己仍有内容，也不能作为渲染源（否则详情面板永久停在
/// 下线那一刻的内容）。
#[test]
fn test_resolve_selected_subagent_prefers_live_detail_over_frozen_group() {
    let mut frozen = make_subagent("bg-agent", "coder");
    frozen.view_models = im::Vector::from(vec![text_unit("first")]);
    frozen.is_running = true;
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(frozen)]),
        generation: 0,
    };
    let live = std::collections::HashMap::from([(
        "task-1".to_string(),
        live_detail_for("bg-agent", "instance-bg-agent", "first+second"),
    )]);
    let display = vec![bg_display_entry("task-1", "bg-agent")];

    // 消息区 Enter 写入组 instance_id。
    let by_instance =
        super::resolve_selected_subagent(&snap, &live, &display, Some("instance-bg-agent"))
            .expect("live detail via instance id");
    assert_eq!(
        nested_texts(&by_instance),
        vec!["first+second".to_string()],
        "instance_id 选中应解析到 live detail 内容"
    );
    assert_eq!(by_instance.instance_id, "task-1");

    // 底栏行点击写入绑定的 agent_id。
    let by_agent = super::resolve_selected_subagent(&snap, &live, &display, Some("bg-agent"))
        .expect("live detail via agent id");
    assert_eq!(nested_texts(&by_agent), vec!["first+second".to_string()]);
}

/// [回归] resume 复用 `agent_id`（child_thread_id），但组是**新** occurrence：
/// 前台恢复的那次运行必须渲染自己的组，不能被旧的后台明细顶掉——按 agent_id
/// 回查 live 明细正是这种错配的来源。
#[test]
fn test_resolve_selected_subagent_keeps_foreground_resume_group() {
    let mut resumed = make_subagent("bg-agent", "coder");
    resumed.instance_id = "subagent-2".into();
    resumed.is_running = true;
    resumed.view_models = im::Vector::from(vec![text_unit("resumed in foreground")]);
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(resumed)]),
        generation: 0,
    };
    // 先前以后台方式跑过的那次：BG_DISPLAY 绑定与 live 明细都还在。
    let live = std::collections::HashMap::from([(
        "task-1".to_string(),
        live_detail_for("bg-agent", "subagent-1", "background run"),
    )]);
    let display = vec![bg_display_entry("task-1", "bg-agent")];

    let group = super::resolve_selected_subagent(&snap, &live, &display, Some("subagent-2"))
        .expect("foreground resume group");
    assert_eq!(
        nested_texts(&group),
        vec!["resumed in foreground".to_string()],
        "新 occurrence 不得被同 agent_id 的旧后台明细顶掉"
    );
    assert_eq!(group.instance_id, "subagent-2");
}

/// 同一 agent 的两次后台运行并存时，选中哪一次就解析哪一次——不能都落到最新 task。
#[test]
fn test_resolve_selected_subagent_matches_selected_occurrence() {
    let snap = ViewModelsSnapshot::default();
    let live = std::collections::HashMap::from([
        (
            "task-old".to_string(),
            live_detail_for("bg-agent", "subagent-1", "old run"),
        ),
        (
            "task-new".to_string(),
            live_detail_for("bg-agent", "subagent-2", "new run"),
        ),
    ]);
    let display = vec![
        bg_display_entry("task-old", "bg-agent"),
        bg_display_entry("task-new", "bg-agent"),
    ];

    let older = super::resolve_selected_subagent(&snap, &live, &display, Some("subagent-1"))
        .expect("older occurrence");
    assert_eq!(older.instance_id, "task-old");
    assert_eq!(nested_texts(&older), vec!["old run".to_string()]);

    let newer = super::resolve_selected_subagent(&snap, &live, &display, Some("subagent-2"))
        .expect("newer occurrence");
    assert_eq!(newer.instance_id, "task-new");
    assert_eq!(nested_texts(&newer), vec!["new run".to_string()]);

    // 底栏点击使用 task_id；同一 agent 的旧行和新行各自打开自己的记录。
    let older_row = super::resolve_selected_subagent(&snap, &live, &display, Some("task-old"))
        .expect("older task row");
    let newer_row = super::resolve_selected_subagent(&snap, &live, &display, Some("task-new"))
        .expect("newer task row");
    assert_eq!(nested_texts(&older_row), vec!["old run".to_string()]);
    assert_eq!(nested_texts(&newer_row), vec!["new run".to_string()]);
}

/// 同步 subagent 不在 `BG_LIVE_DETAIL` 中——仍走 VIEW_MODELS 扫描（不得回归）。
#[test]
fn test_resolve_selected_subagent_falls_back_to_view_models_for_sync_group() {
    let sync = TuiRenderUnit::TuiSubAgentGroup(make_subagent("sync-agent", "Sync"));
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![sync]),
        generation: 0,
    };
    let live = std::collections::HashMap::from([(
        "task-1".to_string(),
        live_detail_for("bg-agent", "subagent-1", "bg text"),
    )]);
    let display = vec![bg_display_entry("task-1", "bg-agent")];

    let found =
        super::resolve_selected_subagent(&snap, &live, &display, Some("instance-sync-agent"))
            .expect("sync group from view models");
    assert_eq!(found.agent_id, "sync-agent");
    assert_eq!(found.instance_id, "instance-sync-agent");
    assert!(super::resolve_selected_subagent(&snap, &live, &display, Some("nope")).is_none());
}

use crate::kit::atoms::ViewModelsSnapshot;
