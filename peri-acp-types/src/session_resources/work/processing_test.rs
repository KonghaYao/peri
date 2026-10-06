use super::*;
use crate::messages::BaseMessage;

fn fixture(stage: WorkStage) -> (WorkState, WorkCommand, WorkTarget, WorkReceipt) {
    let target = WorkTarget {
        work_id: "original".into(),
        expected_work_revision: 0,
    };
    let command = WorkCommand {
        session_id: "session".into(),
        recipient_lifecycle: 1,
        mutation_id: "settlement".into(),
        action: WorkAction::SettleWork {
            expected_revision: 0,
            target: target.clone(),
        },
    };
    let receipt = WorkReceipt {
        session_id: "session".into(),
        mutation_id: "settlement".into(),
        before_revision: 0,
        revision: 0,
        decision: WorkDecision::Accepted,
        delivery_id: None,
        admission_sequence: None,
        batch_id: None,
        work_id: None,
        work_revision: None,
        stage: None,
    };
    let mut state = WorkState::default();
    state.works.insert(
        "original".into(),
        WorkRecord {
            work_id: "original".into(),
            revision: 0,
            budget_id: "budget".into(),
            batch_id: "batch".into(),
            stage,
            resume_stage: Some(WorkStage::ActReady),
            request_id: None,
            reason_request: None,
            response: None,
            invocation_ids: vec!["invocation".into()],
            reason: Some("stop evidence".into()),
            recovery_condition: Some("explicit authorization".into()),
        },
    );
    state.invocations.insert(
        "invocation".into(),
        InvocationRecord {
            intent: InvocationIntent {
                invocation_id: "invocation".into(),
                tool_call_id: "call".into(),
                tool_name: "effect".into(),
                arguments_json: "{}".into(),
                arguments_digest: "digest".into(),
                effective_tool_name: "effect".into(),
                effective_arguments_json: "{}".into(),
                effective_arguments_digest: "digest".into(),
                owner_identity: "original-owner".into(),
                scope_id: "scope".into(),
                scope_epoch: Some(1),
                authorization_ref: "auth".into(),
                recovery_locator: "locator".into(),
            },
            recipient_lifecycle: 1,
            work_id: Some("original".into()),
            status: InvocationStatus::DispatchAccepted,
            outcome: None,
            unknown_reason: None,
        },
    );
    (state, command, target, receipt)
}

fn result() -> InvocationResult {
    InvocationResult {
        invocation_id: "invocation".into(),
        outcome: InvocationOutcome::Completed {
            result: WorkPayload::from_payload(&PersistedPayload::Message(
                BaseMessage::tool_result("call", "executed outcome"),
            ))
            .unwrap(),
        },
    }
}

#[test]
fn stopped_work_accepts_original_outcome_without_reactivating() {
    for stage in [WorkStage::Blocked, WorkStage::Abandoned] {
        let (mut state, command, target, mut receipt) = fixture(stage);
        let mut projections = Vec::new();
        commit_act(
            &command,
            &mut state,
            &target,
            &[result()],
            None,
            &mut receipt,
            &mut projections,
        )
        .unwrap();
        assert_eq!(state.works["original"].stage, stage);
        assert_eq!(
            state.works["original"].reason.as_deref(),
            Some("stop evidence")
        );
        assert_eq!(state.works.len(), 1);
        assert_eq!(
            state.invocations["invocation"].status,
            InvocationStatus::Settled
        );
        assert_eq!(projections.len(), 1);
        assert_eq!(receipt.stage, Some(stage));
        assert_eq!(
            begin_dispatch(
                &mut state,
                &WorkTarget {
                    expected_work_revision: 1,
                    ..target.clone()
                },
                "invocation",
                &mut receipt
            ),
            Err(WorkRejection::InvalidTransition)
        );
    }
}

#[test]
fn stopped_work_rejects_successor_before_settlement() {
    for stage in [WorkStage::Blocked, WorkStage::Abandoned] {
        let (mut state, command, target, mut receipt) = fixture(stage);
        let before = state.clone();
        assert_eq!(
            commit_act(
                &command,
                &mut state,
                &target,
                &[result()],
                Some("successor"),
                &mut receipt,
                &mut Vec::new()
            ),
            Err(WorkRejection::InvalidTransition)
        );
        assert_eq!(state, before);
    }
}

#[test]
fn abandoned_unknown_is_accounted_and_can_be_resolved_without_replay() {
    let (mut state, command, mut target, mut receipt) = fixture(WorkStage::Abandoned);
    unknown(
        &mut state,
        &target,
        "invocation",
        "owner disconnected",
        &mut receipt,
    )
    .unwrap();
    assert_eq!(state.works["original"].stage, WorkStage::Abandoned);
    assert_eq!(
        state.invocations["invocation"].status,
        InvocationStatus::OutcomeUnknown
    );
    assert_eq!(
        state.invocations["invocation"].unknown_reason.as_deref(),
        Some("owner disconnected")
    );
    target.expected_work_revision = 1;
    commit_act(
        &command,
        &mut state,
        &target,
        &[result()],
        None,
        &mut receipt,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(state.works["original"].stage, WorkStage::Abandoned);
    assert_eq!(state.invocations["invocation"].unknown_reason, None);
    assert_eq!(
        commit_act(
            &command,
            &mut state,
            &WorkTarget {
                expected_work_revision: 2,
                ..target
            },
            &[result()],
            None,
            &mut receipt,
            &mut Vec::new()
        ),
        Err(WorkRejection::InvalidTransition)
    );
}

fn execution() -> ControlAttempt {
    ControlAttempt {
        turn_id: crate::session::TurnId::new(),
        attempt_id: crate::identity::AttemptId::new(),
    }
}

fn reducer_fixture() -> (WorkState, WorkCommand, ControlState, WorkReceipt) {
    let (mut state, mut command, target, receipt) = fixture(WorkStage::Abandoned);
    let original_execution = execution();
    state.batches.insert(
        "batch".into(),
        ProcessingBatch {
            batch_id: "batch".into(),
            delivery_ids: vec!["delivery".into()],
            processing_delivery_ids: vec!["delivery".into()],
            projection_versions: BTreeMap::new(),
            execution: original_execution.clone(),
            recipient_lifecycle: 1,
        },
    );
    command.action = WorkAction::CommitAct {
        guard: WorkGuard {
            expected_revision: state.revision,
            expected_control_generation: 0,
            execution: original_execution,
        },
        target,
        results: vec![result()],
        next_work_id: None,
    };
    let control = ControlState {
        control_generation: 1,
        attempt: Some(execution()),
        ..ControlState::default()
    };
    (state, command, control, receipt)
}

fn register_recovered_execution(
    state: &mut WorkState,
    receipt: &WorkReceipt,
    recovered_execution: ControlAttempt,
    batch_id: &str,
) {
    let mut recovered_source = state.works["original"].clone();
    recovered_source.work_id = "recovered-source".into();
    recovered_source.batch_id = batch_id.into();
    recovered_source.stage = WorkStage::Settled;
    recovered_source.invocation_ids.clear();
    state
        .works
        .insert(recovered_source.work_id.clone(), recovered_source);
    let mut entered = receipt.clone();
    entered.work_id = Some("recovered-source".into());
    entered.work_revision = Some(0);
    state.admissions.insert(
        "recovered-admission".into(),
        AdmissionRecord {
            admission: WorkAdmission {
                session_id: "session".into(),
                admission_id: "recovered-admission".into(),
                instance_id: "recovered-instance".into(),
                generation_id: "recovered-generation".into(),
                lifecycle: 1,
                control_generation: 0,
                work_id: "recovered-source".into(),
                work_revision: 0,
                execution: recovered_execution,
            },
            entering_receipt: Some(entered),
            settled_receipt: None,
            evidence_id: None,
        },
    );
}

fn assert_reducer_rejected_without_effect(
    state: &WorkState,
    command: &WorkCommand,
    control: &ControlState,
) {
    let reduction = reduce_work(command, control, state).unwrap();
    assert!(matches!(
        reduction.receipt.decision,
        WorkDecision::Rejected { .. }
    ));
    assert_eq!(&reduction.state, state);
    assert!(reduction.projections.is_empty());
    assert!(reduction.events.is_empty());
    assert!(reduction.control.is_none());
}

#[test]
fn reducer_accepts_abandoned_original_execution_settlement_after_control_changes() {
    let (state, command, control, _) = reducer_fixture();
    let reduction = reduce_work(&command, &control, &state).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    assert_eq!(reduction.receipt.stage, Some(WorkStage::Abandoned));
    assert_eq!(
        reduction.state.works["original"].stage,
        WorkStage::Abandoned
    );
    assert_eq!(
        reduction.state.works["original"].reason,
        state.works["original"].reason
    );
    assert_eq!(reduction.state.works.len(), state.works.len());
    assert_eq!(
        reduction.state.invocations["invocation"].status,
        InvocationStatus::Settled
    );
    assert_eq!(reduction.projections.len(), 1);
    assert!(reduction.control.is_none());
}

#[test]
fn reducer_accepts_registered_recovered_execution_for_original_batch_after_control_changes() {
    let (mut state, mut command, control, receipt) = reducer_fixture();
    let recovered_execution = execution();
    register_recovered_execution(&mut state, &receipt, recovered_execution.clone(), "batch");
    let WorkAction::CommitAct { guard, .. } = &mut command.action else {
        unreachable!();
    };
    guard.execution = recovered_execution;
    assert_ne!(guard.execution, state.batches["batch"].execution);
    let reduction = reduce_work(&command, &control, &state).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    assert_eq!(reduction.receipt.stage, Some(WorkStage::Abandoned));
    assert_eq!(reduction.state.works.len(), state.works.len());
    assert_eq!(
        reduction.state.invocations["invocation"].status,
        InvocationStatus::Settled
    );
    assert_eq!(reduction.projections.len(), 1);
    assert!(reduction.control.is_none());
}

#[test]
fn reducer_rejects_unregistered_execution_and_registered_execution_from_wrong_batch() {
    let (mut state, mut command, control, receipt) = reducer_fixture();
    let unrelated_execution = execution();
    let WorkAction::CommitAct { guard, .. } = &mut command.action else {
        unreachable!();
    };
    guard.execution = unrelated_execution.clone();
    assert_reducer_rejected_without_effect(&state, &command, &control);
    let mut wrong_batch = state.batches["batch"].clone();
    wrong_batch.batch_id = "wrong-batch".into();
    wrong_batch.delivery_ids = vec!["other-delivery".into()];
    wrong_batch.processing_delivery_ids = vec!["other-delivery".into()];
    wrong_batch.execution = unrelated_execution.clone();
    state
        .batches
        .insert(wrong_batch.batch_id.clone(), wrong_batch);
    register_recovered_execution(&mut state, &receipt, unrelated_execution, "wrong-batch");
    assert_reducer_rejected_without_effect(&state, &command, &control);
}

#[test]
fn reducer_rejects_abandoned_successor_even_with_current_execution_authority() {
    let (state, mut command, mut control, _) = reducer_fixture();
    let WorkAction::CommitAct {
        guard,
        next_work_id,
        ..
    } = &mut command.action
    else {
        unreachable!();
    };
    *next_work_id = Some("forbidden-successor".into());
    let original_execution = guard.execution.clone();
    let original_control_generation = guard.expected_control_generation;
    assert_reducer_rejected_without_effect(&state, &command, &control);
    control.attempt = Some(original_execution);
    control.control_generation = original_control_generation;
    assert_reducer_rejected_without_effect(&state, &command, &control);
}

#[test]
fn reducer_original_execution_settlement_cannot_cross_lifecycle() {
    let (state, command, mut control, _) = reducer_fixture();
    control.lifecycle = 2;
    assert_reducer_rejected_without_effect(&state, &command, &control);
}
