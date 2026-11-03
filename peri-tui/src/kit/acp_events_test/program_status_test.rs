//! OSC 7501 上报值随 turn / 交互状态变化（AppShell 负责实际写 stdout）。

use super::*;
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
    make_fold_test_state()
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
