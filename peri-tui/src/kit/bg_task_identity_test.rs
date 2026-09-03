use super::*;
use crate::kit::atoms::{BG_AGENT_IDS, BG_DISPLAY, BG_TASK_IDENTITY, BgDisplayEntry};
use serial_test::serial;

fn reset_bg_atoms() {
    crate::kit::atoms::init_atoms();
    BG_DISPLAY.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    BG_AGENT_IDS.state().write().clear();
}

#[test]
#[serial]
fn test_bind_linked_agent_id_on_bg_subagent_started() {
    reset_bg_atoms();
    BG_DISPLAY.state().write().push(BgDisplayEntry {
        id: "task-1".into(),
        linked_agent_id: None,
        agent_type: "agent".into(),
        desc: "job".into(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: std::time::Instant::now(),
        completed_at: None,
    });
    upsert_identity_from_started("task-1", "agent", "job", None);
    let bound = bind_linked_agent_on_subagent_started("agent-99", "coder");
    assert_eq!(bound.as_deref(), Some("task-1"));
    assert_eq!(
        BG_DISPLAY.state().read()[0].linked_agent_id.as_deref(),
        Some("agent-99")
    );
}

#[test]
#[serial]
fn test_resolve_selected_id_prefers_linked_agent_id() {
    reset_bg_atoms();
    let entry = BgDisplayEntry {
        id: "task-1".into(),
        linked_agent_id: Some("agent-linked".into()),
        agent_type: "agent".into(),
        desc: String::new(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: std::time::Instant::now(),
        completed_at: None,
    };
    assert_eq!(
        resolve_subagent_id_for_display(&entry).as_deref(),
        Some("agent-linked")
    );
}

#[test]
#[serial]
fn test_resolve_fallback_single_bg_agent_id() {
    reset_bg_atoms();
    BG_AGENT_IDS.state().write().insert("only-agent".into());
    let entry = BgDisplayEntry {
        id: "task-1".into(),
        linked_agent_id: None,
        agent_type: "agent".into(),
        desc: String::new(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: std::time::Instant::now(),
        completed_at: None,
    };
    assert_eq!(
        resolve_subagent_id_for_display(&entry).as_deref(),
        Some("only-agent")
    );
}

#[test]
#[serial]
fn test_resolve_unbound_does_not_invent_id() {
    reset_bg_atoms();
    BG_AGENT_IDS.state().write().insert("a1".into());
    BG_AGENT_IDS.state().write().insert("a2".into());
    let entry = BgDisplayEntry {
        id: "task-1".into(),
        linked_agent_id: None,
        agent_type: "agent".into(),
        desc: String::new(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: std::time::Instant::now(),
        completed_at: None,
    };
    assert!(resolve_subagent_id_for_display(&entry).is_none());
}
