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
