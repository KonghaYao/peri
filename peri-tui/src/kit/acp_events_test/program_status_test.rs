//! OSC 7501 上报值随 turn / 交互状态变化（AppShell 负责实际写 stdout）。

use super::*;
use crate::kit::acp_types::BgTaskSnapshotSource;
use crate::kit::program_status::{BlockedKind, ProgramStatus};

fn current() -> ProgramStatus {
    *crate::kit::atoms::PROGRAM_STATUS.state().read()
}

fn hitl_event() -> AcpEventData {
    AcpEventData::HitlPending(crate::kit::acp_types::PendingInteraction {
        owner: Default::default(),
        request_id_json: serde_json::to_string(&"hitl-1").unwrap(),
        payload: HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "cargo test"}),
            batch: None,
        },
    })
}

fn ask_user_event() -> AcpEventData {
    AcpEventData::AskUser(crate::kit::acp_types::PendingInteraction {
        owner: Default::default(),
        request_id_json: serde_json::to_string(&"ask-1").unwrap(),
        payload: AskUser {
            questions: vec![Question {
                id: "q1".into(),
                header: String::new(),
                question: "Which?".into(),
                options: vec![QuestionOption {
                    label: "A".into(),
                    description: String::new(),
                }],
                multi_select: false,
            }],
        },
    })
}

fn interaction_terminal() -> AcpEventData {
    AcpEventData::InteractionTerminal {
        owner: Default::default(),
        outcome: crate::acp_client::InteractionUiOutcome::Resolved {
            result: "allow".into(),
        },
    }
}

fn reset() -> BridgeState {
    crate::kit::atoms::PROGRAM_STATUS.set(ProgramStatus::Idle);
    crate::kit::atoms::BG_TASKS.set(Vec::new());
    crate::kit::atoms::BG_LIVE_DETAIL.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    make_fold_test_state()
}

fn bg_entry(task_id: &str, kind: &str, status: Option<&str>) -> crate::kit::acp_types::BgTaskEntry {
    crate::kit::acp_types::BgTaskEntry {
        child_thread_id: None,
        task_id: task_id.into(),
        kind: kind.into(),
        summary: "worker".into(),
        started_at: String::new(),
        pid: None,
        revision: None,
        status: status.map(str::to_owned),
    }
}

fn set_bg_tasks(tasks: Vec<crate::kit::acp_types::BgTaskEntry>) {
    crate::kit::atoms::BG_LIVE_DETAIL.state().write().clear();
    // 活跃任务必须有 live 证据才算 async agent：沿生产 started 路径 seed Running。
    for task in &tasks {
        if crate::kit::bg_task_status::BgTaskStatus::classify(task.status.as_deref()).is_active() {
            crate::kit::bg_task_live::seed_live_from_started(
                &task.task_id,
                &task.kind,
                &task.summary,
                task.pid,
            );
        }
    }
    crate::kit::atoms::BG_TASKS.set(tasks);
}

fn bg_completed(task_id: &str, kind: &str, success: bool) -> AcpEventData {
    AcpEventData::BgTaskCompleted {
        task_id: task_id.into(),
        kind: Some(kind.into()),
        success,
        duration_ms: 1,
        output_preview: None,
        revision: None,
    }
}

fn bg_cancelled(task_id: &str) -> AcpEventData {
    AcpEventData::BgTaskCancelled {
        task_id: task_id.into(),
        reason: "user".into(),
        revision: None,
    }
}

fn bg_snapshot(
    tasks: Vec<crate::kit::acp_types::BgTaskEntry>,
    revision: u64,
    source: BgTaskSnapshotSource,
) -> AcpEventData {
    AcpEventData::BgTaskSnapshot {
        tasks,
        revision: Some(revision),
        source,
    }
}

fn prompt(state: &mut BridgeState, request_id: &str) {
    dispatch_and_notify(
        state,
        &AcpEventData::PromptSubmitted {
            request_id: Some(request_id.into()),
        },
    );
}

#[test]
#[serial]
fn prompt_lifecycle_reports_working_then_done() {
    let mut state = reset();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );
    assert_eq!(current(), ProgramStatus::Working);

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Done);
}

#[test]
#[serial]
fn reason_started_resumes_suspended_request_before_output_and_keeps_cancel_identity() {
    let mut state = reset();
    prompt(&mut state, "same-request");
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert_eq!(state.phase, SessionPhase::Idle);
    assert!(!crate::kit::atoms::ACP_STATE.state().read().is_loading);
    dispatch_and_notify(&mut state, &AcpEventData::ReasonStarted);
    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert!(crate::kit::atoms::ACP_STATE.state().read().is_loading);
    assert_eq!(current(), ProgramStatus::Working);
    assert_eq!(state.current_request_id.as_deref(), Some("same-request"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("same-request".into()),
        },
    );
    assert_eq!(state.phase, SessionPhase::Idle);
    assert!(state.current_request_id.is_none());
    dispatch_and_notify(&mut state, &AcpEventData::ReasonStarted);
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "settled requests must not revive"
    );
    assert!(!crate::kit::atoms::ACP_STATE.state().read().is_loading);
}

#[test]
#[serial]
fn reason_started_cannot_turn_history_replay_into_a_live_execution() {
    let mut state = reset();
    prompt(&mut state, "old-request");
    dispatch_and_notify(&mut state, &AcpEventData::SessionReplayStarted);
    dispatch_and_notify(&mut state, &AcpEventData::ReasonStarted);
    assert_eq!(state.phase, SessionPhase::ReplayingHistory);
    assert!(!crate::kit::atoms::ACP_STATE.state().read().is_loading);
    assert_eq!(state.current_request_id.as_deref(), Some("old-request"));
}

#[test]
#[serial]
fn pending_interactions_report_blocked_and_resume_working() {
    let mut state = reset();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );

    dispatch_and_notify(&mut state, &hitl_event());
    assert_eq!(current(), ProgramStatus::Blocked(BlockedKind::Permission));

    dispatch_and_notify(&mut state, &interaction_terminal());
    assert_eq!(current(), ProgramStatus::Working);

    dispatch_and_notify(&mut state, &ask_user_event());
    assert_eq!(current(), ProgramStatus::Blocked(BlockedKind::Question));

    dispatch_and_notify(&mut state, &interaction_terminal());
    assert_eq!(current(), ProgramStatus::Working);

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Done);
}

#[test]
#[serial]
fn cancel_reports_idle_and_prompt_failure_reports_error() {
    let mut state = reset();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("r1".into()),
        },
    );
    assert_eq!(current(), ProgramStatus::Idle);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r2".into()),
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptFailed {
            request_id: "r2".into(),
        },
    );
    assert_eq!(current(), ProgramStatus::Error);
}

#[test]
#[serial]
fn turn_done_keeps_working_while_async_agent_runs() {
    let mut state = reset();
    set_bg_tasks(vec![bg_entry("agent-1", "agent", Some("running"))]);

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert_eq!(current(), ProgramStatus::Working);
}

#[test]
#[serial]
fn turn_done_with_running_shell_reports_done() {
    let mut state = reset();
    set_bg_tasks(vec![bg_entry("shell-1", "shell", Some("running"))]);

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert_eq!(current(), ProgramStatus::Done);
}

#[test]
#[serial]
fn interrupted_and_suspended_keep_working_while_async_agent_runs() {
    let mut state = reset();
    set_bg_tasks(vec![bg_entry("agent-1", "agent", Some("running"))]);

    prompt(&mut state, "r1");
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("r1".into()),
        },
    );
    assert_eq!(current(), ProgramStatus::Working);

    prompt(&mut state, "r2");
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert_eq!(current(), ProgramStatus::Working);
}

#[test]
#[serial]
fn last_async_agent_completion_exits_working_to_done() {
    let mut state = reset();
    set_bg_tasks(vec![
        bg_entry("agent-1", "agent", Some("running")),
        bg_entry("agent-2", "agent", Some("running")),
    ]);

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Working);

    dispatch_and_notify(&mut state, &bg_completed("agent-1", "agent", true));
    assert_eq!(current(), ProgramStatus::Working, "仍有 agent 在跑");

    dispatch_and_notify(&mut state, &bg_completed("agent-2", "agent", true));
    assert_eq!(current(), ProgramStatus::Done);
}

#[test]
#[serial]
fn last_async_agent_cancellation_exits_working_to_idle() {
    let mut state = reset();
    set_bg_tasks(vec![bg_entry("agent-1", "agent", Some("running"))]);

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Working);

    dispatch_and_notify(&mut state, &bg_cancelled("agent-1"));
    assert_eq!(current(), ProgramStatus::Idle);
}

#[test]
#[serial]
fn agent_completion_while_new_turn_runs_keeps_working() {
    let mut state = reset();
    set_bg_tasks(vec![bg_entry("agent-1", "agent", Some("running"))]);

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Working);

    // bg 回调已启动新 turn：完成事件到达时 phase 仍是 PromptRunning，交给该
    // turn 自己结算，不能提前落到 done。
    prompt(&mut state, "r2");
    dispatch_and_notify(&mut state, &bg_completed("agent-1", "agent", true));
    assert_eq!(current(), ProgramStatus::Working);

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Done);
}

#[test]
#[serial]
fn async_agent_does_not_override_blocked_or_error() {
    let mut state = reset();
    set_bg_tasks(vec![bg_entry("agent-1", "agent", Some("running"))]);

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &hitl_event());
    assert_eq!(current(), ProgramStatus::Blocked(BlockedKind::Permission));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Blocked(BlockedKind::Permission));

    prompt(&mut state, "r2");
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptFailed {
            request_id: "r2".into(),
        },
    );
    assert_eq!(current(), ProgramStatus::Error);
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Error);
}

#[test]
#[serial]
fn historical_running_agent_snapshot_does_not_keep_working() {
    let mut state = reset();
    dispatch_and_notify(
        &mut state,
        &bg_snapshot(
            vec![bg_entry("agent-1", "agent", Some("running"))],
            1,
            BgTaskSnapshotSource::Historical,
        ),
    );

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert_eq!(current(), ProgramStatus::Done);
}

#[test]
#[serial]
fn live_snapshot_removing_last_agent_exits_working_to_done() {
    let mut state = reset();
    dispatch_and_notify(
        &mut state,
        &bg_snapshot(
            vec![bg_entry("agent-1", "agent", Some("running"))],
            1,
            BgTaskSnapshotSource::Live,
        ),
    );

    prompt(&mut state, "r1");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(current(), ProgramStatus::Working);

    dispatch_and_notify(
        &mut state,
        &bg_snapshot(Vec::new(), 2, BgTaskSnapshotSource::Live),
    );
    assert_eq!(current(), ProgramStatus::Done);
}

#[test]
#[serial]
fn cancelled_shell_has_cancelled_live_detail_and_feedback() {
    let mut state = reset();
    crate::kit::atoms::NOTIFICATION.set(None);
    set_bg_tasks(vec![bg_entry("shell-cancel", "shell", Some("running"))]);
    dispatch_and_notify(&mut state, &bg_cancelled("shell-cancel"));
    assert!(crate::kit::atoms::BG_TASKS.state().read().is_empty());
    assert_eq!(
        crate::kit::atoms::BG_LIVE_DETAIL.state().read()["shell-cancel"].status,
        crate::kit::atoms::BgLiveStatus::Cancelled
    );
    let notice = crate::kit::atoms::NOTIFICATION.state();
    let notice = notice.read();
    let message = &notice.as_ref().expect("cancellation feedback").message;
    assert!(message.contains("shell-cancel"));
    assert!(!message.contains("app-note-bg-task-cancelled"));
}
