use serial_test::serial;

use super::*;

#[test]
#[serial]
fn test_session_boundary_clears_both_interaction_surfaces() {
    let old_active = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_hitl = atoms::HITL_PENDING.state().read().clone();
    let old_ask = atoms::ASK_USER_PENDING.state().read().clone();
    project_session_boundary(Some("target"));
    assert_eq!(atoms::ACTIVE_SESSION_ID.state().read().as_str(), "target");
    assert!(atoms::HITL_PENDING.state().read().is_none());
    assert!(atoms::ASK_USER_PENDING.state().read().is_none());
    *atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    *atoms::HITL_PENDING.state().write() = old_hitl;
    *atoms::ASK_USER_PENDING.state().write() = old_ask;
}

#[test]
#[serial]
fn test_session_boundary_clears_read_only_marker() {
    use peri_acp_types::workspace::ReadOnlyAdmission;

    let old_active = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_read_only = atoms::SESSION_READ_ONLY.state().read().clone();
    let old_reset = atoms::BRIDGE_RESET_COUNTER.get();
    atoms::SESSION_READ_ONLY.set(Some(ReadOnlyAdmission::ExecutionBusy));

    project_session_boundary(Some("next"));

    assert!(
        atoms::SESSION_READ_ONLY.state().read().is_none(),
        "会话边界必须清空上一条会话的只读原因，否则新会话会带着别人的只读标记"
    );

    *atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    atoms::SESSION_READ_ONLY.set(old_read_only);
    atoms::BRIDGE_RESET_COUNTER.set(old_reset);
}

#[test]
#[serial]
fn test_session_boundary_clears_goal_projection_and_panel() {
    let old_active = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = atoms::BRIDGE_RESET_COUNTER.get();
    let old_goal = atoms::GOAL_SNAPSHOT.state().read().clone();
    let old_active_panel = *atoms::ACTIVE_PANEL.state().read();
    let old_open_panels = atoms::OPEN_PANELS.state().read().clone();
    *atoms::GOAL_SNAPSHOT.state().write() = Some(atoms::GoalSnapshot {
        objective: Some("old goal".into()),
        status: Some(peri_acp_types::goal::GoalStatus::Active),
        continuation_count: 4,
        ..Default::default()
    });
    *atoms::OPEN_PANELS.state().write() = vec![PanelKind::Goal];
    *atoms::ACTIVE_PANEL.state().write() = Some(PanelKind::Goal);

    project_session_boundary(Some("next"));

    assert!(atoms::GOAL_SNAPSHOT.state().read().is_none());
    assert!(!atoms::OPEN_PANELS.state().read().contains(&PanelKind::Goal));
    assert_ne!(*atoms::ACTIVE_PANEL.state().read(), Some(PanelKind::Goal));

    *atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *atoms::GOAL_SNAPSHOT.state().write() = old_goal;
    *atoms::ACTIVE_PANEL.state().write() = old_active_panel;
    *atoms::OPEN_PANELS.state().write() = old_open_panels;
}

#[test]
#[serial]
fn session_switch_removes_previous_session_background_tasks() {
    use crate::kit::acp_types::BgTaskEntry;
    use crate::kit::atoms::{BgDisplayEntry, BgLiveDetail};
    use std::time::Instant;

    let old_active = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = atoms::BRIDGE_RESET_COUNTER.get();
    let old_tasks = atoms::BG_TASKS.state().read().clone();
    let old_display = atoms::BG_DISPLAY.state().read().clone();
    let old_live = atoms::BG_LIVE_DETAIL.state().read().clone();
    let old_selected = atoms::SELECTED_BG_TASK_ID.state().read().clone();

    atoms::BG_TASKS.state().write().push(BgTaskEntry {
        task_id: "previous-shell".into(),
        kind: "shell".into(),
        summary: "sleep 10".into(),
        started_at: String::new(),
        pid: None,
        revision: None,
        status: None,
    });
    atoms::BG_DISPLAY.state().write().push(BgDisplayEntry {
        id: "previous-shell".into(),
        linked_agent_id: None,
        agent_type: "shell".into(),
        desc: "sleep 10".into(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: Instant::now(),
        completed_at: None,
    });
    atoms::BG_LIVE_DETAIL
        .state()
        .write()
        .insert("previous-shell".into(), BgLiveDetail::default());
    *atoms::SELECTED_BG_TASK_ID.state().write() = Some("previous-shell".into());

    project_session_boundary(Some("next-session"));

    assert!(atoms::BG_TASKS.state().read().is_empty());
    assert!(atoms::BG_DISPLAY.state().read().is_empty());
    assert!(atoms::BG_LIVE_DETAIL.state().read().is_empty());
    assert!(atoms::SELECTED_BG_TASK_ID.state().read().is_none());

    *atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *atoms::BG_TASKS.state().write() = old_tasks;
    *atoms::BG_DISPLAY.state().write() = old_display;
    *atoms::BG_LIVE_DETAIL.state().write() = old_live;
    *atoms::SELECTED_BG_TASK_ID.state().write() = old_selected;
}

#[test]
#[serial]
fn same_session_replay_keeps_running_background_task() {
    use crate::kit::acp_types::BgTaskEntry;

    let old_active = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = atoms::BRIDGE_RESET_COUNTER.get();
    let old_tasks = atoms::BG_TASKS.state().read().clone();
    *atoms::ACTIVE_SESSION_ID.state().write() = "current".into();
    atoms::BG_TASKS.state().write().push(BgTaskEntry {
        task_id: "still-running".into(),
        kind: "shell".into(),
        summary: "long command".into(),
        started_at: String::new(),
        pid: None,
        revision: None,
        status: None,
    });

    project_session_boundary(Some("current"));

    assert!(
        atoms::BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "still-running")
    );

    *atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *atoms::BG_TASKS.state().write() = old_tasks;
}
