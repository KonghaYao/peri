use super::*;
use crate::kit::atoms::{
    BG_AGENT_IDS, BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASK_REVISION, BG_TASKS,
    PROGRAM_STATUS,
};
use crate::kit::bg_task_identity::task_id_for_agent_id;
use crate::kit::program_status::ProgramStatus;

fn reset() -> BridgeState {
    BG_AGENT_IDS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    BG_TASKS.state().write().clear();
    BG_TASK_REVISION.set(None);
    PROGRAM_STATUS.set(ProgramStatus::Idle);
    make_fold_test_state()
}

fn task_started(state: &mut BridgeState, task: &str, child: &str) {
    let entry = serde_json::from_value(json!({
        "task_id": task, "kind": "agent", "summary": "same worker", "started_at": "",
        "child_thread_id": child,
    }))
    .unwrap();
    dispatch_and_notify(state, &AcpEventData::BgTaskStarted(entry));
}

fn child_started(state: &mut BridgeState, child: &str) {
    dispatch_and_notify(
        state,
        &AcpEventData::SubagentStarted {
            agent_id: child.into(),
            agent_name: "same-worker".into(),
            is_background: true,
            parent_tool_call_id: Some(format!("spawn-{child}")),
        },
    );
}

#[test]
#[serial]
fn concurrent_background_children_use_causal_ids_not_started_arrival_order() {
    let mut state = reset();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("parent".into()),
        },
    );
    task_started(&mut state, "task-left", "child-left");
    task_started(&mut state, "task-right", "child-right");
    child_started(&mut state, "child-left");
    child_started(&mut state, "child-right");
    assert_eq!(
        task_id_for_agent_id("child-left").as_deref(),
        Some("task-left")
    );
    assert_eq!(
        task_id_for_agent_id("child-right").as_deref(),
        Some("task-right")
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child-right".into(),
            result: "right finished".into(),
            is_error: false,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskCompleted {
            task_id: "task-right".into(),
            kind: Some("agent".into()),
            success: true,
            duration_ms: 1,
            output_preview: Some("right finished".into()),
            revision: None,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert!(
        BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "task-left")
    );
    assert_eq!(*PROGRAM_STATUS.state().read(), ProgramStatus::Working);
}

#[test]
#[serial]
fn background_task_started_after_child_event_keeps_the_exact_child_identity() {
    let mut state = reset();
    child_started(&mut state, "child-left");
    task_started(&mut state, "task-right", "child-right");
    task_started(&mut state, "task-left", "child-left");
    assert_eq!(
        task_id_for_agent_id("child-left").as_deref(),
        Some("task-left")
    );
    assert_eq!(
        task_id_for_agent_id("child-right").as_deref(),
        Some("task-right")
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child-left".into(),
            result: "left finished".into(),
            is_error: false,
        },
    );
    assert!(
        BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "task-right")
    );
    assert!(
        !BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "task-left")
    );
}
