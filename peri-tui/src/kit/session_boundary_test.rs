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
