//! Tests
use super::*;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::tui_render_unit::{TuiCollapsedGroup, TuiSubAgentGroup, TuiUserBubble};

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
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    }
}

#[test]
fn test_subagent_detail_content_height_exposes_scroll_overflow() {
    let viewport_height = 20u16;
    let content_height = scroll_content_height(80);
    let bottom_offset = content_height.saturating_sub(viewport_height);

    assert_eq!(content_height, 80);
    assert!(content_height > viewport_height);
    assert_eq!(bottom_offset + viewport_height, content_height);
}

#[test]
fn test_subagent_detail_content_height_is_non_zero_and_saturating() {
    assert_eq!(scroll_content_height(0), 1);
    assert_eq!(scroll_content_height(usize::MAX), u16::MAX);
}

#[test]
fn virtual_detail_offset_preserves_full_height_and_clamps_after_resize() {
    assert_eq!(clamp_detail_offset(90_000, 100_000, 20), 90_000);
    assert_eq!(clamp_detail_offset(100_000, 100_000, 20), 99_980);
    assert_eq!(clamp_detail_offset(99_980, 100_000, 100), 99_900);
    assert_eq!(clamp_detail_offset(50, 10, 20), 0);
}

#[test]
fn detail_viewport_uses_panel_bounds_once() {
    assert_eq!(
        detail_viewport(Rect::new(4, 3, 60, 12)),
        Rect::new(4, 4, 59, 10)
    );
}

#[test]
fn detail_width_budget_matches_drawn_viewport() {
    for width in [2, 3, 6, 7, 30, 40, 60, 100, 120] {
        let viewport = detail_viewport(Rect::new(0, 0, width, 12));
        let grid = GridSpec::grid_for(width);
        assert!(grid.line_width() <= viewport.width, "panel width {width}");
        if width >= 7 {
            assert!(
                grid.first_prefix_width() + grid.content_width() <= viewport.width as usize,
                "panel width {width}"
            );
        }
    }
}

#[test]
fn growing_detail_follows_only_when_already_at_bottom() {
    assert_eq!(follow_detail_offset(20, 30, 10, 35, 10, true), 25);
    assert_eq!(follow_detail_offset(12, 30, 10, 35, 10, true), 12);
    assert_eq!(follow_detail_offset(20, 30, 10, 35, 10, false), 20);
    assert_eq!(follow_detail_offset(20, 30, 10, 15, 10, true), 5);
}

#[test]
fn running_detail_opens_at_live_tail_and_completed_detail_opens_at_header() {
    assert_eq!(initial_detail_offset(true, 30, 10), 20);
    assert_eq!(initial_detail_offset(false, 30, 10), 0);
}

#[test]
fn detail_scrollbar_drag_reaches_both_ends_without_thumb_jump() {
    let top = ScrollbarGeometry::new(100, 10, 0);
    assert!(top.contains_thumb(0));
    assert_eq!(top.offset_for(0, top.grab_offset(0)), 0);
    assert_eq!(top.offset_for(7, top.grab_offset(7)), 90);
    let middle = ScrollbarGeometry::new(100, 10, 45);
    assert!(middle.contains_thumb(middle.thumb_start));
    assert!(middle.offset_for(middle.thumb_start, middle.grab_offset(middle.thumb_start)) <= 45);
    assert_eq!(middle.offset_for(7, 0), 90);
}

#[test]
fn test_find_selected_subagent_none_when_no_selection() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
            "alpha", "Alpha",
        ))]),
        generation: 0,
    };
    assert!(find_selected_subagent(&snap, None).is_none());
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
    let g = find_selected_subagent(&snap, Some("beta")).expect("应匹配 beta");
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

    let found = find_selected_subagent(&snap, Some("occurrence-2")).expect("resumed occurrence");
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
    assert!(find_selected_subagent(&snap, Some("nope")).is_none());
}

#[test]
fn test_find_selected_subagent_recurses_into_collapsed_group() {
    // TuiCollapsedGroup 内嵌 SubAgent（分组后 subagent 行被压入组内）——仍可找到
    let collapsed = TuiCollapsedGroup {
        title: "batch".to_string(),
        count: 1,
        failed_count: 0,
        view_models: vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
            "hidden", "Hidden",
        ))],
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        content_hash: 0,
    };
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiCollapsedGroup(collapsed)]),
        generation: 0,
    };
    let g = find_selected_subagent(&snap, Some("hidden")).expect("应找到组内 subagent");
    assert_eq!(g.agent_id, "hidden");
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
    let g = find_selected_subagent(&snap, Some("inner")).expect("应找到嵌套内层");
    assert_eq!(g.agent_id, "inner");
    let outer_match = find_selected_subagent(&snap, Some("outer")).expect("外层也可匹配");
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

    let found =
        find_live_detail_subagent(&live, &display, Some("shared-agent")).expect("latest task");
    assert_eq!(found.instance_id, "task-a-latest");
    assert_eq!(found.agent_name, "Latest");
}

#[test]
fn test_find_selected_subagent_skips_non_subagent_vms() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "hi".to_string(),
            content_hash: 0,
            reminder: None,
            source: None,
        })]),
        generation: 0,
    };
    assert!(find_selected_subagent(&snap, Some("alpha")).is_none());
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
    let by_instance = resolve_selected_subagent(&snap, &live, &display, Some("instance-bg-agent"))
        .expect("live detail via instance id");
    assert_eq!(
        nested_texts(&by_instance),
        vec!["first+second".to_string()],
        "instance_id 选中应解析到 live detail 内容"
    );
    assert_eq!(by_instance.instance_id, "task-1");

    // 底栏行点击写入绑定的 agent_id。
    let by_agent = resolve_selected_subagent(&snap, &live, &display, Some("bg-agent"))
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

    let group = resolve_selected_subagent(&snap, &live, &display, Some("subagent-2"))
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

    let older = resolve_selected_subagent(&snap, &live, &display, Some("subagent-1"))
        .expect("older occurrence");
    assert_eq!(older.instance_id, "task-old");
    assert_eq!(nested_texts(&older), vec!["old run".to_string()]);

    let newer = resolve_selected_subagent(&snap, &live, &display, Some("subagent-2"))
        .expect("newer occurrence");
    assert_eq!(newer.instance_id, "task-new");
    assert_eq!(nested_texts(&newer), vec!["new run".to_string()]);

    // 底栏点击使用 task_id；同一 agent 的旧行和新行各自打开自己的记录。
    let older_row = resolve_selected_subagent(&snap, &live, &display, Some("task-old"))
        .expect("older task row");
    let newer_row = resolve_selected_subagent(&snap, &live, &display, Some("task-new"))
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

    let found = resolve_selected_subagent(&snap, &live, &display, Some("instance-sync-agent"))
        .expect("sync group from view models");
    assert_eq!(found.agent_id, "sync-agent");
    assert_eq!(found.instance_id, "instance-sync-agent");
    assert!(resolve_selected_subagent(&snap, &live, &display, Some("nope")).is_none());
}
