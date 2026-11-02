use super::*;
use crate::kit::atoms::{
    BG_AGENT_IDS, BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS, BgDisplayEntry,
};
use serial_test::serial;

fn reset_bg_atoms() {
    crate::kit::atoms::init_atoms();
    BG_DISPLAY.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    BG_AGENT_IDS.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASKS.state().write().clear();
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
    let bound = bind_linked_agent_on_subagent_started("agent-99", "coder", Some("run"));
    assert_eq!(bound.as_deref(), Some("task-1"));
    assert_eq!(
        BG_DISPLAY.state().read()[0].linked_agent_id.as_deref(),
        Some("agent-99")
    );
}

#[test]
#[serial]
fn test_resolve_selected_id_keeps_clicked_task_identity() {
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
        Some("task-1")
    );
}

#[test]
#[serial]
fn test_resolve_unbound_agent_row_uses_task_id() {
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
        Some("task-1")
    );
}

#[test]
#[serial]
fn test_resolve_unbound_uses_task_id_even_with_multiple_agents() {
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
    assert_eq!(
        resolve_subagent_id_for_display(&entry).as_deref(),
        Some("task-1")
    );
}

#[test]
#[serial]
fn resumed_agent_events_update_latest_background_run() {
    reset_bg_atoms();
    let now = std::time::Instant::now();
    for (task_id, active) in [("old-task", false), ("new-task", true)] {
        BG_DISPLAY.state().write().push(BgDisplayEntry {
            id: task_id.into(),
            linked_agent_id: Some("shared-agent".into()),
            agent_type: "agent".into(),
            desc: "job".into(),
            current_tool: None,
            tool_count: 0,
            is_active: active,
            is_error: false,
            created_at: now,
            completed_at: (!active).then_some(now),
        });
    }

    crate::kit::bg_task_live::init_agent_live_detail(
        "new-task",
        "shared-agent",
        "coder",
        Some("new-occurrence"),
    );
    crate::kit::bg_task_live::append_bg_text_chunk(
        "shared-agent",
        &crate::kit::stream_data::TuiTextChunk {
            text: "new run output".into(),
            agent_id: Some("shared-agent".into()),
            message_id: None,
        },
    );
    crate::kit::bg_task_live::publish_pending_streams();

    assert_eq!(
        task_id_for_agent_id("shared-agent").as_deref(),
        Some("new-task")
    );
    let live = BG_LIVE_DETAIL.state();
    let details = live.read();
    assert!(details.get("old-task").is_none());
    let new = details.get("new-task").expect("new run detail");
    assert_eq!(new.nested_units.len(), 1);
}

#[test]
#[serial]
fn identity_refresh_and_duplicate_bind_preserve_the_current_agent_mapping() {
    reset_bg_atoms();
    let now = peri_time::monotonic_now();
    for id in ["first", "other"] {
        BG_DISPLAY.state().write().push(BgDisplayEntry {
            id: id.into(),
            linked_agent_id: None,
            agent_type: "agent".into(),
            desc: "task".into(),
            current_tool: None,
            tool_count: 0,
            is_active: true,
            is_error: false,
            created_at: now,
            completed_at: None,
        });
        upsert_identity_from_started(id, "agent", "task", None);
    }
    assert_eq!(
        bind_linked_agent_on_subagent_started("child", "coder", Some("first-run")).as_deref(),
        Some("other")
    );
    crate::kit::bg_task_live::init_agent_live_detail("other", "child", "coder", Some("first-run"));
    upsert_identity_from_started("other", "agent", &"工作🙂".repeat(8192), None);
    assert_eq!(
        bind_linked_agent_on_subagent_started("child", "coder", Some("first-run")).as_deref(),
        Some("other")
    );
    let identities = BG_TASK_IDENTITY.state();
    let identities = identities.read();
    let identity = &identities["other"];
    assert_eq!(identity.agent_id.as_deref(), Some("child"));
    assert_eq!(identity.agent_name.as_deref(), Some("coder"));
    assert!(identity.summary.capacity() <= METADATA_TEXT_BYTES);
    assert!(identity.summary.ends_with('…'));
    assert!(BG_DISPLAY.state().read()[0].linked_agent_id.is_none());
}

#[test]
#[serial]
fn resumed_child_binds_new_task_before_the_previous_bg_completion_arrives() {
    reset_bg_atoms();
    let now = peri_time::monotonic_now();
    let row = |id: &str| BgDisplayEntry {
        id: id.into(),
        linked_agent_id: None,
        agent_type: "agent".into(),
        desc: "task".into(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: now,
        completed_at: None,
    };
    BG_DISPLAY.state().write().push(row("old-task"));
    upsert_identity_from_started("old-task", "agent", "old", None);
    assert_eq!(
        bind_linked_agent_on_subagent_started("child", "coder", Some("old-run")).as_deref(),
        Some("old-task")
    );
    crate::kit::bg_task_live::init_agent_live_detail("old-task", "child", "coder", Some("old-run"));
    crate::kit::bg_task_live::handle_bg_subagent_stopped("child", "done", false);
    assert!(BG_DISPLAY.state().read()[0].is_active);

    BG_DISPLAY.state().write().push(row("new-task"));
    upsert_identity_from_started("new-task", "agent", "new", None);
    assert_eq!(
        bind_linked_agent_on_subagent_started("child", "coder", Some("new-run")).as_deref(),
        Some("new-task")
    );
    crate::kit::bg_task_live::init_agent_live_detail("new-task", "child", "coder", Some("new-run"));
    assert_eq!(task_id_for_agent_id("child").as_deref(), Some("new-task"));
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["old-task"].status,
        BgLiveStatus::Succeeded
    );
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["new-task"].status,
        BgLiveStatus::Running
    );
}
