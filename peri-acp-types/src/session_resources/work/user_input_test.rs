use super::*;
use crate::identity::AttemptId;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::TurnId;

fn attempt() -> ControlAttempt {
    ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

fn add_work(state: &mut WorkState, work_id: &str, lifecycle: u64, execution: ControlAttempt) {
    state.batches.insert(
        work_id.into(),
        ProcessingBatch {
            batch_id: work_id.into(),
            delivery_ids: Vec::new(),
            processing_delivery_ids: Vec::new(),
            projection_versions: BTreeMap::new(),
            execution,
            recipient_lifecycle: lifecycle,
        },
    );
    state.works.insert(
        work_id.into(),
        WorkRecord {
            work_id: work_id.into(),
            revision: 7,
            budget_id: work_id.into(),
            batch_id: work_id.into(),
            stage: WorkStage::Blocked,
            resume_stage: Some(WorkStage::ReasonReady),
            request_id: None,
            reason_request: None,
            response: None,
            invocation_ids: Vec::new(),
            reason: Some("reason budget exhausted".into()),
            recovery_condition: Some("explicit budget reset authorization".into()),
        },
    );
    state.budgets.insert(
        work_id.into(),
        WorkBudget {
            reason_requests: 64,
            dispatches: 76,
            recoveries: 0,
        },
    );
    state.obligations.insert(
        work_id.into(),
        RequiredObligation {
            delivery_id: work_id.into(),
            status: ObligationStatus::Blocked,
            work_id: Some(work_id.into()),
            reason: Some("reason budget exhausted".into()),
        },
    );
}

fn publication(control: &ControlState, state: &mut WorkState) -> WorkCommand {
    let message_id = MessageId::new();
    let content = MessageContent::text("continue with this new instruction");
    let input = UserInput {
        input_id: message_id.as_uuid().to_string(),
        content: content.clone(),
        original_draft: "continue with this new instruction".into(),
    };
    let staged = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: "stage".into(),
            action: WorkAction::StageUserInput {
                input_json: serde_json::to_string(&input).unwrap(),
                command_id: "stage-input".into(),
                fingerprint: 1,
            },
        },
        control,
        state,
    )
    .unwrap();
    assert_eq!(staged.receipt.decision, WorkDecision::Accepted);
    *state = staged.state;
    WorkCommand {
        session_id: "session".into(),
        recipient_lifecycle: control.lifecycle,
        mutation_id: "explicit-selection".into(),
        action: WorkAction::PublishStagedUserInputs {
            expected_revision: state.revision,
            expected_control_generation: control.control_generation,
            expected_attempt: control.attempt.clone(),
            interrupt_current: true,
            deliveries: vec![PublishDelivery {
                delivery_id: "new-input".into(),
                event: WorkEvent {
                    producer_namespace: "user-input".into(),
                    event_id: input.input_id,
                    event_kind: "user-input".into(),
                    causation_id: None,
                    content: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::Human {
                            id: message_id,
                            content,
                        },
                    ))
                    .unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            }],
        },
    }
}

fn add_invocation(state: &mut WorkState, invocation_id: &str, status: InvocationStatus) {
    let outcome = (status == InvocationStatus::Settled).then(|| InvocationOutcome::Completed {
        result: WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::Human {
            id: MessageId::new(),
            content: MessageContent::text("already committed result"),
        }))
        .unwrap(),
    });
    state.invocations.insert(
        invocation_id.into(),
        InvocationRecord {
            intent: InvocationIntent {
                invocation_id: invocation_id.into(),
                tool_call_id: invocation_id.into(),
                tool_name: "external-effect".into(),
                arguments_json: "{}".into(),
                arguments_digest: "digest".into(),
                effective_tool_name: "external-effect".into(),
                effective_arguments_json: "{}".into(),
                effective_arguments_digest: "digest".into(),
                owner_identity: "original-owner".into(),
                scope_id: "original-scope".into(),
                scope_epoch: Some(1),
                authorization_ref: "original-authorization".into(),
                recovery_locator: "original-owner-locator".into(),
            },
            recipient_lifecycle: 1,
            work_id: Some("exited".into()),
            status,
            outcome,
            unknown_reason: (status == InvocationStatus::OutcomeUnknown)
                .then(|| "owner reconciliation required".into()),
        },
    );
    state
        .works
        .get_mut("exited")
        .unwrap()
        .invocation_ids
        .push(invocation_id.into());
}

#[test]
fn explicit_selection_unblocks_exited_work_without_replaying_effects_or_resetting_budget() {
    let control = ControlState::default();
    let mut state = WorkState::default();
    state.limits.reason_requests = 64;
    state.limits.dispatches = 256;
    state.limits.recoveries = 8;
    add_work(&mut state, "exited", 1, attempt());
    add_work(&mut state, "old-lifecycle", 2, attempt());
    add_work(&mut state, "settled", 1, attempt());
    state.works.get_mut("settled").unwrap().stage = WorkStage::Settled;
    add_invocation(&mut state, "completed", InvocationStatus::Settled);
    add_invocation(&mut state, "unknown", InvocationStatus::OutcomeUnknown);
    let command = publication(&control, &mut state);
    let reduction = reduce_work(&command, &control, &state).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    assert_eq!(reduction.state.works["exited"].stage, WorkStage::Abandoned);
    assert_eq!(reduction.state.works["exited"].revision, 8);
    assert_eq!(
        reduction.state.works["old-lifecycle"],
        state.works["old-lifecycle"]
    );
    assert_eq!(reduction.state.works["settled"], state.works["settled"]);
    assert_eq!(reduction.state.invocations, state.invocations);
    assert_eq!(reduction.state.budgets, state.budgets);
    assert_eq!(reduction.state.limits, WorkLimits::default());
    assert_eq!(reduction.state.batches, state.batches);
    assert_eq!(
        reduction.state.obligations["exited"].status,
        ObligationStatus::Abandoned
    );
    assert!(reduction.state.works["exited"]
        .reason
        .as_ref()
        .unwrap()
        .contains(&command.mutation_id));
    assert!(reduction.control.is_none());
    assert!(reduction.projections.is_empty());
    assert_eq!(reduction.events.len(), 1);
    let snapshot = WorkSnapshot::from_state(
        &WorkQuery {
            session_id: "session".into(),
            limit: 10,
        },
        control,
        reduction.state,
    );
    assert!(!snapshot.blocked);
    assert_eq!(snapshot.candidates.len(), 1);
    assert_eq!(snapshot.candidates[0].delivery_ids, vec!["new-input"]);
}

#[test]
fn active_attempt_selection_only_abandons_exact_execution() {
    let control = ControlState {
        attempt: Some(attempt()),
        ..ControlState::default()
    };
    let mut state = WorkState::default();
    state.limits.reason_requests = 64;
    state.limits.dispatches = 256;
    state.limits.recoveries = 9;
    add_work(&mut state, "active", 1, control.attempt.clone().unwrap());
    add_work(&mut state, "other", 1, attempt());
    let mut other_attempt = control.attempt.clone().unwrap();
    other_attempt.attempt_id = AttemptId::new();
    add_work(&mut state, "same-turn-other-attempt", 1, other_attempt);
    let command = publication(&control, &mut state);
    let reduction = reduce_work(&command, &control, &state).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    assert_eq!(reduction.state.limits, state.limits);
    assert_eq!(reduction.state.works["active"].stage, WorkStage::Abandoned);
    for work_id in ["other", "same-turn-other-attempt"] {
        assert_eq!(reduction.state.works[work_id], state.works[work_id]);
        assert_eq!(
            reduction.state.obligations[work_id],
            state.obligations[work_id]
        );
    }
}

#[test]
fn publication_without_explicit_interruption_keeps_exited_processing() {
    let control = ControlState::default();
    let mut state = WorkState::default();
    state.limits.reason_requests = 64;
    state.limits.dispatches = 256;
    state.limits.recoveries = 8;
    add_work(&mut state, "exited", 1, attempt());
    let mut command = publication(&control, &mut state);
    if let WorkAction::PublishStagedUserInputs {
        interrupt_current, ..
    } = &mut command.action
    {
        *interrupt_current = false;
    }
    let reduction = reduce_work(&command, &control, &state).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    assert_eq!(reduction.state.limits, state.limits);
    assert_eq!(reduction.state.works, state.works);
    assert_eq!(
        reduction.state.obligations["exited"],
        state.obligations["exited"]
    );
}

#[test]
fn selection_keeps_lifecycle_generation_cas_attempt_and_idempotence_guards() {
    let control = ControlState::default();
    let mut state = WorkState::default();
    state.limits.reason_requests = 64;
    state.limits.dispatches = 256;
    state.limits.recoveries = 8;
    add_work(&mut state, "exited", 1, attempt());
    let command = publication(&control, &mut state);
    let mut stale_lifecycle = control.clone();
    stale_lifecycle.lifecycle += 1;
    let mut stale_generation = control.clone();
    stale_generation.control_generation += 1;
    let mut active_control = control.clone();
    active_control.attempt = Some(attempt());
    for (changed_control, reason) in [
        (stale_lifecycle, WorkRejection::StaleLifecycle),
        (stale_generation, WorkRejection::StaleControlGeneration),
        (active_control, WorkRejection::StaleExecution),
    ] {
        let rejected = reduce_work(&command, &changed_control, &state).unwrap();
        assert_eq!(rejected.receipt.decision, WorkDecision::Rejected { reason });
        assert_eq!(rejected.state, state);
        assert!(rejected.events.is_empty());
    }
    let mut changed_state = state.clone();
    changed_state.revision += 1;
    let rejected = reduce_work(&command, &control, &changed_state).unwrap();
    assert_eq!(
        rejected.receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleRevision
        }
    );
    assert_eq!(rejected.state, changed_state);
    let accepted = reduce_work(&command, &control, &state).unwrap();
    assert_eq!(accepted.receipt.decision, WorkDecision::Accepted);
    let duplicate = reduce_work(&command, &control, &accepted.state).unwrap();
    assert_eq!(
        duplicate.receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleRevision
        }
    );
    assert_eq!(duplicate.state, accepted.state);
    assert!(duplicate.events.is_empty());
    let mut conflict = command;
    if let WorkAction::PublishStagedUserInputs {
        interrupt_current,
        expected_revision,
        ..
    } = &mut conflict.action
    {
        *interrupt_current = false;
        *expected_revision = accepted.state.revision;
    }
    let rejected = reduce_work(&conflict, &control, &accepted.state).unwrap();
    assert_eq!(
        rejected.receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    assert_eq!(rejected.state, accepted.state);
}
