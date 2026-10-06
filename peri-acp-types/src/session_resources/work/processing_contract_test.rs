use super::*;

#[test]
fn reason_budget_blocks_durably_without_starting_another_request() {
    let mut facts = claimed();
    facts.head.limits.reason_requests = 1;
    facts.processing.as_mut().unwrap().budget.reason_requests = 1;
    let previous = facts.processing.clone().unwrap();
    begin_reason(&mut facts);
    let processing = facts.processing.as_ref().unwrap();
    assert_eq!(processing.stage, WorkStage::Blocked);
    assert_eq!(processing.resume_stage, Some(WorkStage::ReasonReady));
    assert_eq!(
        processing.blocked_evidence.as_deref(),
        Some("reason budget exhausted")
    );
    assert_eq!(processing.budget, previous.budget);
    assert_eq!(processing.request, previous.request);
    assert_eq!(processing.request_id, previous.request_id);
    let restored: WorkFacts = serde_json::from_slice(&serde_json::to_vec(&facts).unwrap()).unwrap();
    assert_eq!(restored, facts);
}

#[test]
fn reason_zero_budget_and_maximum_counter_block_without_overflow() {
    for limit in [0, u64::MAX] {
        let mut facts = claimed();
        facts.head.limits.reason_requests = limit;
        facts.processing.as_mut().unwrap().budget.reason_requests = limit;
        begin_reason(&mut facts);
        let processing = facts.processing.unwrap();
        assert_eq!(processing.stage, WorkStage::Blocked);
        assert_eq!(processing.budget.reason_requests, limit);
    }
}

#[test]
fn abandon_processing_releases_only_its_unfulfilled_inputs() {
    let mut facts = claimed();
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("later-input"),
        }),
    );
    let later = facts
        .deliveries
        .iter()
        .find(|delivery| delivery.delivery_id == "later-input")
        .unwrap()
        .clone();
    let action = WorkAction::AbandonWork {
        expected_revision: facts.head.change_seq,
        expected_control_generation: facts.control.control_generation,
        target: target(&facts),
        reason: "stopped".into(),
        authorization_ref: "stop-command".into(),
    };
    apply(&mut facts, command(action));
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::Abandoned
    );
    assert_eq!(facts.head.current_processing_id, None);
    assert_eq!(facts.head.required_count, 1);
    assert_eq!(
        facts.head.required_bytes,
        later.publication.event.content.content.byte_length
    );
    let owned = facts
        .deliveries
        .iter()
        .find(|delivery| delivery.delivery_id == "delivery")
        .unwrap();
    assert_eq!(owned.obligation, ObligationStatus::Abandoned);
    assert_eq!(owned.disposition.as_deref(), Some("stopped"));
    assert_eq!(
        facts
            .deliveries
            .iter()
            .find(|delivery| delivery.delivery_id == "later-input")
            .unwrap(),
        &later
    );
}

#[test]
fn finish_rejects_another_observed_attempt_before_writing_leaving_evidence() {
    for attempt in [Some(
        serde_json::from_value(serde_json::json!({
            "turnId": "00000000-0000-4000-8000-000000000002", "attemptId": "other"
        }))
        .unwrap(),
    )] {
        let mut facts = claimed();
        let admission = facts.admission.as_ref().unwrap().admission.clone();
        facts.control.attempt = attempt;
        assert_rejected(
            &facts,
            WorkAction::FinishAdmission {
                admission,
                evidence_id: "finished".into(),
            },
            WorkRejection::StaleExecution,
        );
        assert!(facts.admission.unwrap().leaving_evidence_id.is_none());
    }
}

#[test]
fn finish_original_admission_after_observation_was_cleared() {
    let mut facts = claimed();
    let admission = facts.admission.as_ref().unwrap().admission.clone();
    facts.control.attempt = None;
    apply(
        &mut facts,
        command(WorkAction::FinishAdmission {
            admission,
            evidence_id: "cold-finished".into(),
        }),
    );
    assert_eq!(
        facts.admission.unwrap().leaving_evidence_id.as_deref(),
        Some("cold-finished")
    );
    assert_eq!(facts.head.current_admission_id, None);
}

fn delegated_candidate() -> (WorkFacts, WorkAction, DelegationRef) {
    let mut parent = act_ready();
    dispatch(&mut parent, "first");
    let binding = TaskBinding {
        invocation_id: "first".into(),
        owner_identity: "owner".into(),
        owner_task_id: "child-task".into(),
        initiator_session_id: "session".into(),
        recipient_lifecycle: 1,
        recovery_locator: "locator".into(),
        authorization_ref: "grant".into(),
    };
    let bound = apply(
        &mut parent,
        command(WorkAction::ReconcileTaskBinding {
            expected_revision: 0,
            binding: binding.clone(),
        }),
    );
    let mut child = facts();
    child.control.attempt = None;
    for identity in ["child-first", "child-second"] {
        apply(
            &mut child,
            command(WorkAction::PublishDelivery {
                delivery: publication(identity),
            }),
        );
    }
    child.parent_binding_receipt = Some(bound.receipt.clone());
    child.parent_effect = parent
        .effects
        .iter()
        .find(|effect| effect.invocation_id == "first")
        .cloned();
    let delegation = DelegationRef {
        parent_session_id: "session".into(),
        parent_lifecycle: 1,
        delegation_id: "first".into(),
    };
    let action = WorkAction::BindWorkDelegation {
        expected_revision: child.head.change_seq,
        work_id: pending_processing_id("session", 1, child.head.next_delivery_seq),
        binding,
        parent_binding_receipt: bound.receipt,
    };
    (child, action, delegation)
}

#[test]
fn preclaim_delegation_survives_cold_restore_then_moves_to_sdk_admitted_processing() {
    let (mut facts, action, delegation) = delegated_candidate();
    let work_id = match &action {
        WorkAction::BindWorkDelegation { work_id, .. } => work_id.clone(),
        _ => unreachable!(),
    };
    apply(&mut facts, command(action.clone()));
    assert!(facts.processing.is_none());
    assert!(facts.control.attempt.is_none());
    assert!(facts.head.current_admission_id.is_none());
    for delivery in &facts.deliveries {
        assert_eq!(delivery.obligation, ObligationStatus::Pending);
        assert_eq!(delivery.processing_id, None);
        assert_eq!(delivery.delegation.as_ref(), Some(&delegation));
    }
    let mut facts: WorkFacts =
        serde_json::from_slice(&serde_json::to_vec(&facts).unwrap()).unwrap();
    apply(&mut facts, command(action));
    let admission = WorkAdmission {
        session_id: "session".into(),
        admission_id: "sdk-admission".into(),
        instance_id: "instance".into(),
        generation_id: "generation".into(),
        lifecycle: 1,
        control_generation: 0,
        work_id: work_id.clone(),
        work_revision: 0,
        execution: super::facts().control.attempt.unwrap(),
    };
    apply(
        &mut facts,
        command(WorkAction::RegisterAdmission { admission }),
    );
    let action = WorkAction::ClaimBatch {
        guard: guard(&facts),
        batch_id: work_id,
        delivery_ids: facts
            .deliveries
            .iter()
            .map(|delivery| delivery.delivery_id.clone())
            .collect(),
    };
    apply(&mut facts, command(action));
    assert_eq!(
        facts.processing.as_ref().unwrap().delegation.as_ref(),
        Some(&delegation)
    );
    assert!(facts
        .deliveries
        .iter()
        .all(|delivery| delivery.delegation.is_none()));
}

#[test]
fn preclaim_delegation_rejects_inexact_candidate_and_missing_or_conflicting_parent_proof() {
    let (facts, action, _) = delegated_candidate();
    let mut wrong_candidate = action.clone();
    if let WorkAction::BindWorkDelegation { work_id, .. } = &mut wrong_candidate {
        *work_id = "batch:session:1:999".into();
    }
    assert_rejected(&facts, wrong_candidate, WorkRejection::Conflict);
    let mut missing = facts.clone();
    missing.parent_effect = None;
    assert_rejected(&missing, action.clone(), WorkRejection::Conflict);
    let mut claimed = facts.clone();
    claimed.deliveries[0].processing_id = Some("other-processing".into());
    assert_rejected(&claimed, action.clone(), WorkRejection::Conflict);
    let mut conflicting = facts.clone();
    conflicting.deliveries[0].delegation = Some(DelegationRef {
        parent_session_id: "other".into(),
        parent_lifecycle: 1,
        delegation_id: "other".into(),
    });
    assert_rejected(&conflicting, action, WorkRejection::Conflict);
}

#[test]
fn recovery_budget_preserves_original_resume_stage_and_exact_request() {
    let mut facts = claimed();
    begin_reason(&mut facts);
    let action = WorkAction::BlockWork {
        expected_revision: facts.head.change_seq,
        target: target(&facts),
        reason: "model unavailable".into(),
        recovery_condition: "provider restored".into(),
    };
    apply(&mut facts, command(action));
    facts.head.limits.recoveries = 0;
    let exact_request = facts.processing.as_ref().unwrap().request.clone();
    let action = WorkAction::ResumeWork {
        guard: guard(&facts),
        target: target(&facts),
        recovery_evidence: "provider restored".into(),
    };
    apply(&mut facts, command(action));
    let processing = facts.processing.unwrap();
    assert_eq!(processing.stage, WorkStage::Blocked);
    assert_eq!(processing.resume_stage, Some(WorkStage::ReasonInFlight));
    assert_eq!(processing.request, exact_request);
    assert_eq!(processing.budget.recoveries, 0);
    assert_eq!(
        processing.blocked_evidence.as_deref(),
        Some("recovery budget exhausted")
    );
}

#[test]
fn text_only_response_retains_explicit_continuation_and_cumulative_budget() {
    let mut facts = claimed();
    for phase_sequence in 1..=2 {
        begin_reason(&mut facts);
        let previous = facts.processing.as_ref().unwrap().clone();
        let action = WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&facts),
            target: target(&facts),
            request_id: "request".into(),
            response: payload("partial-response", "assistant"),
            dispatch_intents: Vec::new(),
            next_work_id: Some(previous.processing_id.clone()),
        };
        apply(&mut facts, command(action));
        let processing = facts.processing.as_ref().unwrap();
        assert_eq!(processing.processing_id, previous.processing_id);
        assert_eq!(processing.stage, WorkStage::ReasonReady);
        assert_eq!(processing.phase_sequence, phase_sequence);
        assert_eq!(processing.budget, previous.budget);
        assert_eq!(processing.request, None);
        assert_eq!(processing.request_id, None);
        assert_eq!(
            facts.head.current_processing_id.as_ref(),
            Some(&processing.processing_id)
        );
        assert_eq!(facts.deliveries[0].obligation, ObligationStatus::Satisfied);
        assert_eq!(facts.head.required_count, 0);
    }
}

#[test]
fn text_only_final_response_settles_without_an_implicit_continuation() {
    let mut facts = claimed();
    begin_reason(&mut facts);
    let action = WorkAction::CommitReasonResponseAndDispatchIntent {
        guard: guard(&facts),
        target: target(&facts),
        request_id: "request".into(),
        response: payload("final-response", "assistant"),
        dispatch_intents: Vec::new(),
        next_work_id: None,
    };
    apply(&mut facts, command(action));
    assert_eq!(facts.processing.as_ref().unwrap().stage, WorkStage::Settled);
    assert_eq!(facts.head.current_processing_id, None);
    assert_eq!(facts.head.required_count, 0);
}

#[test]
fn finish_evidence_replays_after_exact_attempt_was_cleared() {
    let mut facts = claimed();
    let admission = facts.admission.as_ref().unwrap().admission.clone();
    let action = WorkAction::FinishAdmission {
        admission,
        evidence_id: "finished".into(),
    };
    apply(&mut facts, command(action.clone()));
    assert_eq!(facts.control.attempt, None);
    apply(&mut facts, command(action));
    assert_eq!(
        facts.admission.unwrap().leaving_evidence_id.as_deref(),
        Some("finished")
    );
}

#[test]
fn dispatch_budget_block_survives_settlement_of_every_effect() {
    for limit in [0, 1] {
        let mut facts = act_ready();
        facts.head.limits.dispatches = limit;
        if limit == 1 {
            dispatch(&mut facts, "first");
        }
        dispatch(&mut facts, "second");
        let blocked = facts.processing.as_ref().unwrap().clone();
        assert_eq!(blocked.stage, WorkStage::Blocked);
        for identity in ["first", "second"] {
            let effect = facts
                .effects
                .iter()
                .find(|effect| effect.invocation_id == identity)
                .unwrap();
            let outcome = if effect.status == InvocationStatus::Prepared {
                InvocationOutcome::Cancelled {
                    evidence: format!(
                        "before-effect:processing-stopped-before-dispatch:{identity}"
                    ),
                    result: payload(identity, "tool"),
                }
            } else {
                InvocationOutcome::Completed {
                    result: payload(identity, "tool"),
                }
            };
            let action = WorkAction::CommitAct {
                guard: guard(&facts),
                target: target(&facts),
                results: vec![InvocationResult {
                    invocation_id: identity.into(),
                    expected_effect_revision: effect.revision,
                    outcome,
                }],
                next_work_id: None,
            };
            apply(&mut facts, command(action));
        }
        let processing = facts.processing.as_ref().unwrap();
        assert_eq!(processing.stage, WorkStage::Blocked);
        assert_eq!(processing.blocked_evidence, blocked.blocked_evidence);
        assert_eq!(processing.recovery_condition, blocked.recovery_condition);
        assert_eq!(processing.resume_stage, blocked.resume_stage);
        assert_eq!(processing.budget, blocked.budget);
        assert_eq!(processing.remaining_effects, 0);
        assert_eq!(facts.head.unresolved_effects, 0);
        assert_eq!(
            facts.head.current_processing_id.as_ref(),
            Some(&processing.processing_id)
        );
    }
}

#[test]
fn unknown_outcome_reconciliation_still_advances_after_all_effects_settle() {
    for continue_processing in [false, true] {
        let mut facts = act_ready();
        dispatch(&mut facts, "first");
        dispatch(&mut facts, "second");
        let action = WorkAction::OutcomeUnknown {
            expected_revision: facts.head.change_seq,
            target: target(&facts),
            invocation_id: "first".into(),
            expected_effect_revision: 1,
            reason: "transport disconnected".into(),
        };
        apply(&mut facts, command(action));
        assert_eq!(facts.processing.as_ref().unwrap().stage, WorkStage::Blocked);
        let action = WorkAction::CommitAct {
            guard: guard(&facts),
            target: target(&facts),
            results: vec![result(&facts, "first"), result(&facts, "second")],
            next_work_id: continue_processing.then(|| work_id(&facts)),
        };
        apply(&mut facts, command(action));
        let processing = facts.processing.as_ref().unwrap();
        assert_eq!(
            processing.stage,
            if continue_processing {
                WorkStage::ReasonReady
            } else {
                WorkStage::Settled
            }
        );
        assert_eq!(processing.resume_stage, None);
        assert_eq!(processing.phase_sequence, 1);
        assert_eq!(processing.remaining_effects, 0);
        assert_eq!(facts.head.unresolved_effects, 0);
        assert_eq!(
            facts.head.current_processing_id.is_some(),
            continue_processing
        );
    }
}

fn registered_candidate(identities: &[&str]) -> WorkFacts {
    let mut facts = facts();
    for identity in identities {
        apply(
            &mut facts,
            command(WorkAction::PublishDelivery {
                delivery: publication(identity),
            }),
        );
    }
    admit(&mut facts);
    facts
}

#[test]
fn registered_candidate_keeps_frozen_members_after_another_producer_publishes() {
    let mut facts = registered_candidate(&["first-input"]);
    let original = facts.admission.as_ref().unwrap().clone();
    assert_eq!(
        original.initial_delivery_ids,
        Some(vec!["first-input".into()])
    );
    assert_eq!(
        original.admission.work_id,
        pending_processing_id("session", 1, 2)
    );
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("late-input"),
        }),
    );
    assert_ne!(
        original.admission.work_id,
        pending_processing_id("session", 1, facts.head.next_delivery_seq)
    );
    apply(
        &mut facts,
        command(WorkAction::RegisterAdmission {
            admission: original.admission.clone(),
        }),
    );
    assert_eq!(facts.admission.as_ref().unwrap(), &original);
    let mut facts: WorkFacts =
        serde_json::from_slice(&serde_json::to_vec(&facts).unwrap()).unwrap();
    let action = WorkAction::ClaimBatch {
        guard: guard(&facts),
        batch_id: original.admission.work_id.clone(),
        delivery_ids: vec!["first-input".into()],
    };
    apply(&mut facts, command(action));
    assert_eq!(
        facts.processing.as_ref().unwrap().processing_id,
        original.admission.work_id
    );
    assert_eq!(facts.processing.as_ref().unwrap().delivery_count, 1);
    let late = facts
        .deliveries
        .iter()
        .find(|delivery| delivery.delivery_id == "late-input")
        .unwrap();
    assert_eq!(late.obligation, ObligationStatus::Pending);
    assert_eq!(late.processing_id, None);
    assert_eq!(facts.admission.as_ref().unwrap(), &original);
}

#[test]
fn claim_rejects_added_missing_or_reordered_frozen_members() {
    let mut facts = registered_candidate(&["first-input", "second-input"]);
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("late-input"),
        }),
    );
    for delivery_ids in [
        vec!["first-input".into()],
        vec!["second-input".into(), "first-input".into()],
        vec![
            "first-input".into(),
            "second-input".into(),
            "late-input".into(),
        ],
    ] {
        assert_rejected(
            &facts,
            WorkAction::ClaimBatch {
                guard: guard(&facts),
                batch_id: work_id(&facts),
                delivery_ids,
            },
            WorkRejection::Conflict,
        );
    }
    assert!(facts.processing.is_none());
    assert!(facts
        .deliveries
        .iter()
        .all(|delivery| delivery.obligation == ObligationStatus::Pending
            && delivery.processing_id.is_none()));
}

#[test]
fn unclaimed_admission_without_frozen_evidence_is_explicitly_legacy_unknown() {
    for initial_delivery_ids in [None, Some(Vec::new())] {
        let mut facts = registered_candidate(&["first-input"]);
        facts.admission.as_mut().unwrap().initial_delivery_ids = initial_delivery_ids;
        assert_rejected(
            &facts,
            WorkAction::ClaimBatch {
                guard: guard(&facts),
                batch_id: work_id(&facts),
                delivery_ids: vec!["first-input".into()],
            },
            WorkRejection::LegacyUnknown,
        );
        assert_rejected(
            &facts,
            WorkAction::RegisterAdmission {
                admission: facts.admission.as_ref().unwrap().admission.clone(),
            },
            WorkRejection::LegacyUnknown,
        );
    }
}

#[test]
fn initial_admission_rejects_an_inexact_candidate_or_nonzero_revision() {
    let mut facts = registered_candidate(&["first-input"]);
    let mut admission = facts.admission.take().unwrap().admission;
    facts.head.current_admission_id = None;
    admission.work_id = "batch:session:1:999".into();
    assert_rejected(
        &facts,
        WorkAction::RegisterAdmission {
            admission: admission.clone(),
        },
        WorkRejection::Conflict,
    );
    admission.work_id = work_id(&facts);
    admission.work_revision = 1;
    assert_rejected(
        &facts,
        WorkAction::RegisterAdmission { admission },
        WorkRejection::StaleWorkRevision,
    );
}

#[test]
fn existing_processing_admission_does_not_recapture_pending_inputs() {
    let mut facts = claimed();
    let original = facts.admission.as_ref().unwrap().admission.clone();
    apply(
        &mut facts,
        command(WorkAction::FinishAdmission {
            admission: original.clone(),
            evidence_id: "sdk-exited".into(),
        }),
    );
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("late-input"),
        }),
    );
    let resumed = WorkAdmission {
        admission_id: "resumed-sdk-admission".into(),
        work_revision: facts.processing.as_ref().unwrap().revision,
        ..original
    };
    facts.admission = None;
    apply(
        &mut facts,
        command(WorkAction::RegisterAdmission { admission: resumed }),
    );
    assert_eq!(facts.admission.as_ref().unwrap().initial_delivery_ids, None);
    assert_eq!(facts.processing.as_ref().unwrap().delivery_count, 1);
    assert_eq!(
        facts
            .deliveries
            .iter()
            .find(|delivery| delivery.delivery_id == "late-input")
            .unwrap()
            .processing_id,
        None
    );
}
