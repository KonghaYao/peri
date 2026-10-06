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

fn add_admission(state: &mut WorkState, control: &ControlState, work_id: &str) {
    state.works.get_mut(work_id).unwrap().stage = WorkStage::ReasonReady;
    let registered = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("register-{work_id}"),
            action: WorkAction::RegisterAdmission {
                admission: WorkAdmission {
                    session_id: "session".into(),
                    admission_id: work_id.into(),
                    instance_id: "instance".into(),
                    generation_id: "generation".into(),
                    lifecycle: control.lifecycle,
                    control_generation: control.control_generation,
                    work_id: work_id.into(),
                    work_revision: state.works[work_id].revision,
                    execution: control.attempt.clone().unwrap(),
                },
            },
        },
        control,
        state.clone(),
    )
    .unwrap();
    assert_eq!(registered.receipt.decision, WorkDecision::Accepted);
    assert_eq!(registered.control.as_ref(), Some(control));
    let registered_state = registered.state.unwrap();
    let blocked = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("block-{work_id}"),
            action: WorkAction::BlockWork {
                expected_revision: registered_state.revision,
                target: WorkTarget {
                    work_id: work_id.into(),
                    expected_work_revision: registered_state.works[work_id].revision,
                },
                reason: "reason budget exhausted".into(),
                recovery_condition: "explicit budget reset authorization".into(),
            },
        },
        control,
        registered_state,
    )
    .unwrap();
    assert_eq!(blocked.receipt.decision, WorkDecision::Accepted);
    *state = blocked.state.unwrap();
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
        state.clone(),
    )
    .unwrap();
    assert_eq!(staged.receipt.decision, WorkDecision::Accepted);
    *state = staged.state.unwrap();
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
    let reduction = reduce_work(&command, &control, state.clone()).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    let accepted = reduction.state.as_ref().unwrap();
    assert_eq!(accepted.works["exited"].stage, WorkStage::Abandoned);
    assert_eq!(accepted.works["exited"].revision, 8);
    assert_eq!(
        accepted.works["old-lifecycle"],
        state.works["old-lifecycle"]
    );
    assert_eq!(accepted.works["settled"], state.works["settled"]);
    assert_eq!(accepted.invocations, state.invocations);
    assert_eq!(accepted.budgets, state.budgets);
    assert_eq!(accepted.limits, WorkLimits::default());
    assert_eq!(accepted.batches, state.batches);
    assert_eq!(
        accepted.obligations["exited"].status,
        ObligationStatus::Abandoned
    );
    assert!(accepted.works["exited"]
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
        reduction.state.unwrap(),
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
    add_admission(&mut state, &control, "active");
    add_work(&mut state, "other", 1, attempt());
    let mut other_attempt = control.attempt.clone().unwrap();
    other_attempt.attempt_id = AttemptId::new();
    add_work(&mut state, "same-turn-other-attempt", 1, other_attempt);
    let command = publication(&control, &mut state);
    let reduction = reduce_work(&command, &control, state.clone()).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    let accepted = reduction.state.as_ref().unwrap();
    assert_eq!(accepted.limits, state.limits);
    assert_eq!(accepted.works["active"].stage, WorkStage::Abandoned);
    for work_id in ["other", "same-turn-other-attempt"] {
        assert_eq!(accepted.works[work_id], state.works[work_id]);
        assert_eq!(accepted.obligations[work_id], state.obligations[work_id]);
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
    let reduction = reduce_work(&command, &control, state.clone()).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    let accepted = reduction.state.as_ref().unwrap();
    assert_eq!(accepted.limits, state.limits);
    assert_eq!(accepted.works, state.works);
    assert_eq!(accepted.obligations["exited"], state.obligations["exited"]);
}

/// [回归测试] A claim 的 batch 在 B recovery 后仍保留 A execution，选择必须跟随 B admission 与 successor。
#[test]
fn recovered_attempt_selection_follows_admission_batch_and_successor() {
    let idle = ControlState::default();
    let mut state = WorkState::default();
    let publish = publication(&idle, &mut state);
    let published = reduce_work(&publish, &idle, state.clone()).unwrap();
    assert_eq!(published.receipt.decision, WorkDecision::Accepted);
    let original_execution = attempt();
    let original_admission = WorkAdmission {
        session_id: "session".into(),
        admission_id: "original-admission".into(),
        instance_id: "original-instance".into(),
        generation_id: "original-generation".into(),
        lifecycle: 1,
        control_generation: 0,
        work_id: "new-input".into(),
        work_revision: 0,
        execution: original_execution.clone(),
    };
    let registered = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: 1,
            mutation_id: "register-original".into(),
            action: WorkAction::RegisterAdmission {
                admission: original_admission,
            },
        },
        &idle,
        published.state.unwrap(),
    )
    .unwrap();
    assert_eq!(registered.receipt.decision, WorkDecision::Accepted);
    let active = registered.control.unwrap();
    let registered_state = registered.state.unwrap();
    let claimed = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: 1,
            mutation_id: "claim-original".into(),
            action: WorkAction::ClaimBatch {
                guard: WorkGuard {
                    expected_revision: registered_state.revision,
                    expected_control_generation: active.control_generation,
                    execution: original_execution.clone(),
                },
                batch_id: "original-batch".into(),
                delivery_ids: vec!["new-input".into()],
            },
        },
        &active,
        registered_state,
    )
    .unwrap();
    assert_eq!(claimed.receipt.decision, WorkDecision::Accepted);
    let recovery_idle = ControlState {
        attempt: None,
        ..active
    };
    let recovered_execution = attempt();
    let recovered = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: 1,
            mutation_id: "register-recovery".into(),
            action: WorkAction::RegisterAdmission {
                admission: WorkAdmission {
                    session_id: "session".into(),
                    admission_id: "recovery-admission".into(),
                    instance_id: "recovery-instance".into(),
                    generation_id: "recovery-generation".into(),
                    lifecycle: 1,
                    control_generation: recovery_idle.control_generation,
                    work_id: "original-batch".into(),
                    work_revision: 0,
                    execution: recovered_execution.clone(),
                },
            },
        },
        &recovery_idle,
        claimed.state.unwrap(),
    )
    .unwrap();
    assert_eq!(recovered.receipt.decision, WorkDecision::Accepted);
    let control = recovered.control.unwrap();
    let mut state = recovered.state.unwrap();
    let mut successor = state.works["original-batch"].clone();
    state.works.get_mut("original-batch").unwrap().stage = WorkStage::Settled;
    successor.work_id = "recovered-successor".into();
    state.works.insert(successor.work_id.clone(), successor);
    add_work(
        &mut state,
        "unassociated-current-execution",
        1,
        recovered_execution,
    );
    add_work(
        &mut state,
        "original-execution-only",
        1,
        original_execution.clone(),
    );
    let invocations = state.invocations.clone();
    let budgets = state.budgets.clone();
    let mut command = publication(&control, &mut state);
    command.mutation_id = "recovery-selection".into();
    if let WorkAction::PublishStagedUserInputs { deliveries, .. } = &mut command.action {
        deliveries[0].delivery_id = "fresh-input".into();
    }
    let reduction = reduce_work(&command, &control, state.clone()).unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    let accepted = reduction.state.as_ref().unwrap();
    assert_eq!(
        accepted.works["recovered-successor"].stage,
        WorkStage::Abandoned
    );
    assert_eq!(
        accepted.works["original-batch"],
        state.works["original-batch"]
    );
    for work_id in ["unassociated-current-execution", "original-execution-only"] {
        assert_eq!(accepted.works[work_id], state.works[work_id]);
        assert_eq!(accepted.obligations[work_id], state.obligations[work_id]);
    }
    assert_eq!(
        accepted.batches["original-batch"].execution,
        original_execution
    );
    assert_eq!(
        accepted.obligations["new-input"].status,
        ObligationStatus::Abandoned
    );
    assert_eq!(accepted.invocations, invocations);
    assert_eq!(accepted.budgets, budgets);
    assert_eq!(accepted.admissions, state.admissions);
    assert_eq!(
        accepted.terminal_acknowledgements,
        state.terminal_acknowledgements
    );
}

/// [回归测试] batch execution 相同不能替代精确当前 admission 授权。
#[test]
fn active_selection_without_current_admission_keeps_processing() {
    for variant in 0..6 {
        let control = ControlState {
            attempt: Some(attempt()),
            ..ControlState::default()
        };
        let mut state = WorkState::default();
        add_work(&mut state, "active", 1, control.attempt.clone().unwrap());
        add_admission(&mut state, &control, "active");
        let admission = &mut state.admissions.get_mut("active").unwrap().admission;
        match variant {
            0 => admission.session_id = "other-session".into(),
            1 => admission.lifecycle += 1,
            2 => admission.control_generation += 1,
            3 => admission.execution.attempt_id = AttemptId::new(),
            4 => admission.work_id = "missing-work".into(),
            5 => state.admissions.clear(),
            _ => unreachable!(),
        }
        let command = publication(&control, &mut state);
        let reduction = reduce_work(&command, &control, state.clone()).unwrap();
        assert_eq!(
            reduction.receipt.decision,
            WorkDecision::Accepted,
            "variant {variant}"
        );
        let accepted = reduction.state.as_ref().unwrap();
        assert_eq!(accepted.works, state.works, "variant {variant}");
        assert_eq!(accepted.obligations["active"], state.obligations["active"]);
    }
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
        let rejected = reduce_work(&command, &changed_control, state.clone()).unwrap();
        assert_eq!(rejected.receipt.decision, WorkDecision::Rejected { reason });
        assert_eq!(rejected.state, None);
        assert!(rejected.events.is_empty());
    }
    let mut changed_state = state.clone();
    changed_state.revision += 1;
    let rejected = reduce_work(&command, &control, changed_state).unwrap();
    assert_eq!(
        rejected.receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleRevision
        }
    );
    assert_eq!(rejected.state, None);
    let accepted = reduce_work(&command, &control, state).unwrap();
    assert_eq!(accepted.receipt.decision, WorkDecision::Accepted);
    let accepted_state = accepted.state.unwrap();
    let duplicate = reduce_work(&command, &control, accepted_state.clone()).unwrap();
    assert_eq!(
        duplicate.receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleRevision
        }
    );
    assert_eq!(duplicate.state, None);
    assert!(duplicate.events.is_empty());
    let mut conflict = command;
    if let WorkAction::PublishStagedUserInputs {
        interrupt_current,
        expected_revision,
        ..
    } = &mut conflict.action
    {
        *interrupt_current = false;
        *expected_revision = accepted_state.revision;
    }
    let rejected = reduce_work(&conflict, &control, accepted_state).unwrap();
    assert_eq!(
        rejected.receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    assert_eq!(rejected.state, None);
}

#[test]
fn explicit_selection_after_stop_supersedes_exact_attempt_from_prior_control_generation() {
    let active = ControlState {
        attempt: Some(attempt()),
        ..ControlState::default()
    };
    let mut state = WorkState::default();
    add_work(&mut state, "stopped", 1, active.attempt.clone().unwrap());
    add_admission(&mut state, &active, "stopped");
    let stopped = crate::session_resources::control::decide_control(
        &crate::session_resources::ControlCommand {
            session_id: "session".into(),
            command_id: "stop-original".into(),
            expected_lifecycle: active.lifecycle,
            expected_revision: active.revision,
            expected_control_generation: active.control_generation,
            action: crate::session_resources::ControlAction::Stop {
                target: active.attempt.clone().unwrap(),
            },
        },
        &active,
    );
    assert_eq!(
        stopped.decision,
        crate::session_resources::ControlDecision::Accepted
    );
    let command = publication(&stopped.state, &mut state);
    let selected = reduce_work(&command, &stopped.state, state).unwrap();
    assert_eq!(selected.receipt.decision, WorkDecision::Accepted);
    assert_eq!(
        selected.state.as_ref().unwrap().works["stopped"].stage,
        WorkStage::Abandoned
    );
    assert_eq!(
        selected.control.as_ref().unwrap().status,
        crate::session_resources::ControlStatus::Active
    );
    assert_eq!(selected.control.as_ref().unwrap().attempt, active.attempt);
}
