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
fn test_collect_subagents_empty_snapshot() {
    let snap = ViewModelsSnapshot::default();
    assert!(collect_subagents(&snap).is_empty());
}

#[test]
fn test_collect_subagents_only_user_bubbles() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "hi".to_string(),
            content_hash: 0,
            reminder: None,
            source: None,
        })]),
        generation: 0,
    };
    assert!(collect_subagents(&snap).is_empty());
}

#[test]
fn test_collect_subagents_dedup_across_committed_and_current() {
    // 同一 agent_id 出现在 items 中两次——应只保留一次
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("researcher", "Researcher")),
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("researcher", "Researcher")),
        ]),
        generation: 0,
    };
    let result = collect_subagents(&snap);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].agent_id, "researcher");
}

#[test]
fn test_collect_subagents_preserves_insertion_order() {
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("alpha", "Alpha")),
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("beta", "Beta")),
            TuiRenderUnit::TuiSubAgentGroup(make_subagent("gamma", "Gamma")),
        ]),
        generation: 0,
    };
    let result = collect_subagents(&snap);
    let ids: Vec<_> = result.iter().map(|s| s.agent_id.as_str()).collect();
    assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_collect_subagents_recurses_into_collapsed_group() {
    // TuiCollapsedGroup 内嵌 SubAgent——应被扫描到
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
    let result = collect_subagents(&snap);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].agent_id, "hidden");
}

#[test]
fn test_collect_subagents_recurses_into_nested_subagent() {
    // SubAgent 内嵌 SubAgent（嵌套）——内层也应被扫描
    let mut outer = make_subagent("outer", "Outer");
    let outer_vms: Vec<TuiRenderUnit> = vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
        "inner", "Inner",
    ))];
    outer.view_models = im::Vector::from(outer_vms);
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(outer)]),
        generation: 0,
    };
    let result = collect_subagents(&snap);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].agent_id, "outer");
    assert_eq!(result[1].agent_id, "inner");
}
