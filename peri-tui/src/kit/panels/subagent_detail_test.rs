//! Tests
use super::*;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::tui_render_unit::{TuiCollapsedGroup, TuiSubAgentGroup, TuiUserBubble};

fn make_subagent(id: &str, name: &str) -> TuiSubAgentGroup {
    TuiSubAgentGroup {
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
