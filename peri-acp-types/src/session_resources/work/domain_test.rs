use super::*;
use crate::session::MessagePolicy;
use crate::session_resources::ControlStatus;

#[path = "lifecycle_test.rs"]
mod lifecycle;

fn payload_ref(identity: &str) -> PayloadRef {
    EvidenceWrite {
        session_id: "session".into(),
        storage_scope: "workspace".into(),
        payload_id: identity.into(),
        encoding: 1,
        bytes: identity.as_bytes().to_vec(),
    }
    .reference()
    .unwrap()
}

fn payload(identity: &str, role: &str) -> WorkPayload {
    WorkPayload {
        message_id: MessageId::new(),
        role: role.into(),
        content: payload_ref(identity),
        tool_call_id: if role == "tool" {
            Some(format!("call-{identity}"))
        } else {
            None
        },
    }
}

fn facts() -> WorkFacts {
    let attempt = serde_json::from_value(serde_json::json!({
        "turnId": "00000000-0000-4000-8000-000000000001", "attemptId": "attempt"
    }))
    .unwrap();
    WorkFacts {
        session_id: "session".into(),
        control: ControlState {
            attempt: Some(attempt),
            ..ControlState::default()
        },
        head: SessionWorkHead::default(),
        processing: None,
        deliveries: Vec::new(),
        effects: Vec::new(),
        drafts: Vec::new(),
        admission: None,
        recovery_descriptor: None,
        terminal_obligation: None,
        legacy_evidence: None,
        parent_binding_receipt: None,
        parent_effect: None,
    }
}

fn command(action: WorkAction) -> WorkCommand {
    WorkCommand {
        session_id: "session".into(),
        recipient_lifecycle: 1,
        mutation_id: uuid::Uuid::new_v4().to_string(),
        action,
    }
}

fn guard(facts: &WorkFacts) -> WorkGuard {
    WorkGuard {
        expected_revision: facts.head.change_seq,
        expected_control_generation: facts.control.control_generation,
        execution: facts.control.attempt.clone().unwrap(),
    }
}

fn target(facts: &WorkFacts) -> WorkTarget {
    let processing = facts.processing.as_ref().unwrap();
    WorkTarget {
        work_id: processing.processing_id.clone(),
        expected_work_revision: processing.revision,
    }
}

fn apply(facts: &mut WorkFacts, command: WorkCommand) -> WorkTransition {
    let transition = transition_work(&command, facts).unwrap();
    assert_eq!(transition.receipt.decision, WorkDecision::Accepted);
    for write in &transition.writes {
        match write {
            WorkWrite::Head { record, .. } => facts.head = record.clone(),
            WorkWrite::Processing { record, .. } => facts.processing = Some(record.clone()),
            WorkWrite::Delivery { record, .. } => {
                facts
                    .deliveries
                    .retain(|prior| prior.delivery_id != record.delivery_id);
                facts.deliveries.push(record.clone());
            }
            WorkWrite::Effect { record, .. } => {
                facts
                    .effects
                    .retain(|prior| prior.invocation_id != record.invocation_id);
                facts.effects.push(record.clone());
            }
            WorkWrite::Draft { record, .. } => {
                facts
                    .drafts
                    .retain(|prior| prior.input_id != record.input_id);
                facts.drafts.push(record.clone());
            }
            WorkWrite::Admission { record, .. } => facts.admission = Some(record.clone()),
            WorkWrite::RecoveryDescriptor { record, .. } => {
                facts.recovery_descriptor = Some(record.clone())
            }
            WorkWrite::TerminalObligation { record, .. } => {
                facts.terminal_obligation = Some(record.clone())
            }
            WorkWrite::LegacyEvidence { record } => facts.legacy_evidence = Some(record.clone()),
            WorkWrite::Control { record, .. } => facts.control = record.clone(),
            WorkWrite::Transcript { .. } => {}
        }
    }
    transition
}

fn assert_rejected(facts: &WorkFacts, action: WorkAction, rejection: WorkRejection) {
    let transition = transition_work(&command(action), facts).unwrap();
    assert_eq!(
        transition.receipt.decision,
        WorkDecision::Rejected { reason: rejection }
    );
    assert!(transition.writes.is_empty());
    assert_eq!(transition.receipt.revision, facts.head.change_seq);
}

fn publication(identity: &str) -> PublishDelivery {
    PublishDelivery {
        delivery_id: identity.into(),
        event: WorkEvent {
            producer_namespace: "test".into(),
            event_id: identity.into(),
            event_kind: "input".into(),
            causation_id: None,
            content: payload(identity, "user"),
        },
        purpose: DeliveryPurpose::UserInput,
        policy: MessagePolicy::ensure_processing(),
    }
}

fn admit(facts: &mut WorkFacts) {
    let admission = WorkAdmission {
        session_id: "session".into(),
        admission_id: "admission".into(),
        instance_id: "instance".into(),
        generation_id: "generation".into(),
        lifecycle: 1,
        control_generation: 0,
        work_id: "processing".into(),
        work_revision: 0,
        execution: facts.control.attempt.clone().unwrap(),
    };
    apply(facts, command(WorkAction::RegisterAdmission { admission }));
}

fn claimed() -> WorkFacts {
    let mut facts = facts();
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("delivery"),
        }),
    );
    admit(&mut facts);
    let action = WorkAction::ClaimBatch {
        guard: guard(&facts),
        batch_id: "processing".into(),
        delivery_ids: vec!["delivery".into()],
    };
    apply(&mut facts, command(action));
    facts
}

fn begin_reason(facts: &mut WorkFacts) {
    let request = payload_ref("exact-provider-request");
    let action = WorkAction::BeginReason {
        guard: guard(facts),
        target: target(facts),
        request_id: "request".into(),
        request: ReasonRequest {
            request_digest: request.sha256.clone(),
            payload: request,
            model_ref: "model".into(),
            authorization_ref: "credential-grant".into(),
        },
    };
    apply(facts, command(action));
}

fn intent(identity: &str) -> InvocationIntent {
    let arguments = payload_ref("model-arguments");
    let effective = payload_ref("approved-arguments");
    InvocationIntent {
        invocation_id: identity.into(),
        tool_call_id: format!("call-{identity}"),
        tool_name: "tool".into(),
        arguments_digest: arguments.sha256.clone(),
        arguments,
        effective_tool_name: "tool".into(),
        effective_arguments_digest: effective.sha256.clone(),
        effective_arguments: effective,
        owner_identity: "owner".into(),
        scope_id: "scope".into(),
        scope_epoch: Some(1),
        authorization_ref: "grant".into(),
        recovery_locator: "locator".into(),
    }
}

fn act_ready() -> WorkFacts {
    let mut facts = claimed();
    begin_reason(&mut facts);
    let action = WorkAction::CommitReasonResponseAndDispatchIntent {
        guard: guard(&facts),
        target: target(&facts),
        request_id: "request".into(),
        response: payload("response", "assistant"),
        dispatch_intents: vec![intent("first"), intent("second")],
        next_work_id: None,
    };
    apply(&mut facts, command(action));
    facts
}

fn dispatch(facts: &mut WorkFacts, identity: &str) {
    let revision = facts
        .effects
        .iter()
        .find(|effect| effect.invocation_id == identity)
        .unwrap()
        .revision;
    let action = WorkAction::BeginDispatch {
        guard: guard(facts),
        target: target(facts),
        invocation_id: identity.into(),
        expected_effect_revision: revision,
    };
    apply(facts, command(action));
}

fn result(facts: &WorkFacts, identity: &str) -> InvocationResult {
    InvocationResult {
        invocation_id: identity.into(),
        expected_effect_revision: facts
            .effects
            .iter()
            .find(|effect| effect.invocation_id == identity)
            .unwrap()
            .revision,
        outcome: InvocationOutcome::Completed {
            result: payload(identity, "tool"),
        },
    }
}

#[test]
fn immutable_evidence_rejects_digest_and_length_changes() {
    let write = EvidenceWrite {
        session_id: "session".into(),
        storage_scope: "workspace".into(),
        payload_id: "body".into(),
        encoding: 1,
        bytes: b"exact bytes".to_vec(),
    };
    let mut record = EvidenceRecord {
        reference: write.reference().unwrap(),
        bytes: write.bytes,
    };
    record.validate().unwrap();
    record.bytes.push(0);
    assert!(record.validate().is_err());
    record.bytes.pop();
    record.reference.sha256 = "0".repeat(64);
    assert!(record.validate().is_err());
}

#[test]
fn delivery_replay_is_exact_and_conflict_has_no_partial_writes() {
    let mut facts = facts();
    let publication = publication("delivery");
    let original = apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication.clone(),
        }),
    );
    let replay = apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication.clone(),
        }),
    );
    assert_eq!(
        replay.receipt.admission_sequence,
        original.receipt.admission_sequence
    );
    assert_eq!(facts.head.required_count, 1);
    assert_eq!(facts.deliveries.len(), 1);
    let mut changed = publication;
    changed.event.content.content = payload_ref("other");
    assert_rejected(
        &facts,
        WorkAction::PublishDelivery { delivery: changed },
        WorkRejection::Conflict,
    );
}

#[test]
fn capacity_rejection_preserves_required_delivery_evidence() {
    let mut facts = facts();
    facts.head.limits.required_deliveries = 0;
    assert_rejected(
        &facts,
        WorkAction::PublishDelivery {
            delivery: publication("required"),
        },
        WorkRejection::Capacity,
    );
    assert_eq!(facts.head.required_count, 0);
    assert!(facts.deliveries.is_empty());
}

#[test]
fn draft_withdrawal_returns_immutable_original_without_publication() {
    let mut facts = facts();
    let content = payload_ref("unaltered-draft");
    apply(
        &mut facts,
        command(WorkAction::StageUserInput {
            input_id: "input".into(),
            content: content.clone(),
            command_id: "stage".into(),
            fingerprint: 10,
        }),
    );
    apply(
        &mut facts,
        command(WorkAction::WithdrawStagedUserInput {
            input_id: "input".into(),
            command_id: "withdraw".into(),
            fingerprint: 11,
        }),
    );
    assert_eq!(facts.drafts[0].content, content);
    assert_eq!(facts.drafts[0].status, StagedUserInputStatus::Withdrawn);
    assert!(facts.deliveries.is_empty());
    assert!(facts.drafts[0].publication_id.is_none());
}

#[test]
fn publication_selection_conflicts_with_take_back_and_does_not_publish_subset() {
    let mut facts = facts();
    let mut delivery = publication("input");
    apply(
        &mut facts,
        command(WorkAction::StageUserInput {
            input_id: "input".into(),
            content: delivery.event.content.content.clone(),
            command_id: "stage".into(),
            fingerprint: 10,
        }),
    );
    delivery.event.causation_id = Some(
        serde_json::to_string(&UserInputPublicationIdentity {
            input_id: "input".into(),
            publication_generation: "publish".into(),
            fingerprint: 12,
            command_id: "publish".into(),
            draft_binding: StagedUserInputPublicationBinding {
                draft_revision: facts.drafts[0].revision,
                draft_fingerprint: facts.drafts[0].fingerprint,
                canonical_content: delivery.event.content.content.clone(),
            },
        })
        .unwrap(),
    );
    apply(
        &mut facts,
        command(WorkAction::WithdrawStagedUserInput {
            input_id: "input".into(),
            command_id: "withdraw".into(),
            fingerprint: 11,
        }),
    );
    assert_rejected(
        &facts,
        WorkAction::PublishStagedUserInputs {
            expected_revision: 0,
            expected_control_generation: 0,
            expected_attempt: facts.control.attempt.clone(),
            interrupt_current: false,
            deliveries: vec![delivery],
        },
        WorkRejection::Conflict,
    );
}

#[test]
fn claimed_delivery_cannot_be_withdrawn_and_membership_is_single_authority() {
    let facts = claimed();
    let delivery = &facts.deliveries[0];
    assert_eq!(delivery.processing_id.as_deref(), Some("processing"));
    assert_eq!(delivery.batch_ordinal, Some(0));
    assert!(delivery.participates_in_reason);
    assert_eq!(facts.processing.as_ref().unwrap().reason_delivery_count, 1);
    assert_rejected(
        &facts,
        WorkAction::WithdrawDelivery {
            expected_revision: 0,
            expected_control_generation: None,
            delivery_id: "delivery".into(),
            authorization_ref: "grant".into(),
        },
        WorkRejection::InvalidTransition,
    );
}

#[test]
fn reason_is_inflight_only_with_exact_request_and_spends_cumulative_budget() {
    let mut facts = claimed();
    begin_reason(&mut facts);
    let processing = facts.processing.as_ref().unwrap();
    assert_eq!(processing.stage, WorkStage::ReasonInFlight);
    assert_eq!(processing.budget.reason_requests, 1);
    assert_eq!(
        processing.request,
        Some(payload_ref("exact-provider-request"))
    );
    assert_eq!(facts.deliveries[0].obligation, ObligationStatus::InProgress);
}

#[test]
fn response_dispatch_intents_and_input_satisfaction_share_one_write_set() {
    let facts = act_ready();
    assert_eq!(facts.head.required_count, 0);
    assert_eq!(facts.head.unresolved_effects, 2);
    assert_eq!(facts.deliveries[0].obligation, ObligationStatus::Satisfied);
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::ActReady
    );
    assert!(facts
        .effects
        .iter()
        .all(|effect| effect.status == InvocationStatus::Prepared));
    assert_ne!(
        facts.effects[0].intent.arguments,
        facts.effects[0].intent.effective_arguments
    );
}

#[test]
fn incomplete_membership_cannot_satisfy_input_or_expose_dispatch() {
    let mut facts = claimed();
    begin_reason(&mut facts);
    facts.deliveries.clear();
    assert_rejected(
        &facts,
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&facts),
            target: target(&facts),
            request_id: "request".into(),
            response: payload("response", "assistant"),
            dispatch_intents: vec![intent("first")],
            next_work_id: None,
        },
        WorkRejection::Conflict,
    );
    assert_eq!(facts.head.required_count, 1);
    assert!(facts.effects.is_empty());
}

#[test]
fn prepared_effect_does_not_settle_without_dispatch_acceptance_barrier() {
    let facts = act_ready();
    assert_rejected(
        &facts,
        WorkAction::CommitAct {
            guard: guard(&facts),
            target: target(&facts),
            results: vec![result(&facts, "first")],
            next_work_id: Some("processing".into()),
        },
        WorkRejection::InvalidTransition,
    );
}

#[test]
fn independent_effect_revisions_allow_stale_processing_observation() {
    let mut facts = act_ready();
    let stale = target(&facts);
    dispatch(&mut facts, "first");
    let action = WorkAction::BeginDispatch {
        guard: guard(&facts),
        target: stale,
        invocation_id: "second".into(),
        expected_effect_revision: 0,
    };
    apply(&mut facts, command(action));
    assert_eq!(facts.processing.as_ref().unwrap().budget.dispatches, 2);
    assert!(facts
        .effects
        .iter()
        .all(|effect| effect.status == InvocationStatus::DispatchAccepted));
}

#[test]
fn unknown_retains_intent_and_prevents_new_dispatch_until_reconciliation() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    let action = WorkAction::OutcomeUnknown {
        expected_revision: 0,
        target: target(&facts),
        invocation_id: "first".into(),
        expected_effect_revision: 1,
        reason: "lost owner ack".into(),
    };
    apply(&mut facts, command(action));
    assert_eq!(
        facts
            .effects
            .iter()
            .find(|effect| effect.invocation_id == "first")
            .unwrap()
            .status,
        InvocationStatus::OutcomeUnknown
    );
    assert_eq!(facts.processing.as_ref().unwrap().stage, WorkStage::Blocked);
    assert_eq!(facts.head.unresolved_effects, 2);
    assert_rejected(
        &facts,
        WorkAction::BeginDispatch {
            guard: guard(&facts),
            target: target(&facts),
            invocation_id: "second".into(),
            expected_effect_revision: 0,
        },
        WorkRejection::InvalidTransition,
    );
}

#[test]
fn results_commit_independently_last_result_advances_same_processing_without_reset() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    dispatch(&mut facts, "second");
    let action = WorkAction::CommitAct {
        guard: guard(&facts),
        target: target(&facts),
        results: vec![result(&facts, "first")],
        next_work_id: Some("processing".into()),
    };
    let first = apply(&mut facts, command(action));
    assert_eq!(facts.processing.as_ref().unwrap().remaining_effects, 1);
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::ActReady
    );
    assert_eq!(
        first
            .writes
            .iter()
            .filter(|write| matches!(write, WorkWrite::Transcript { .. }))
            .count(),
        1
    );
    let action = WorkAction::CommitAct {
        guard: guard(&facts),
        target: target(&facts),
        results: vec![result(&facts, "second")],
        next_work_id: Some("processing".into()),
    };
    apply(&mut facts, command(action));
    let processing = facts.processing.as_ref().unwrap();
    assert_eq!(processing.processing_id, "processing");
    assert_eq!(processing.stage, WorkStage::ReasonReady);
    assert_eq!(processing.phase_sequence, 1);
    assert_eq!(processing.budget.reason_requests, 1);
    assert_eq!(processing.budget.dispatches, 2);
    assert_eq!(facts.head.unresolved_effects, 0);
}

#[test]
fn cancelled_result_must_still_project_a_paired_tool_response() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    let action = WorkAction::CommitAct {
        guard: guard(&facts),
        target: target(&facts),
        results: vec![InvocationResult {
            invocation_id: "first".into(),
            expected_effect_revision: 1,
            outcome: InvocationOutcome::Cancelled {
                evidence: "stopped".into(),
                result: payload("first", "tool"),
            },
        }],
        next_work_id: Some("processing".into()),
    };
    let transition = apply(&mut facts, command(action));
    assert!(transition
        .writes
        .iter()
        .any(|write| matches!(write, WorkWrite::Transcript { payload } if payload.role == "tool")));
}

#[test]
fn abandonment_preserves_unknown_effect_and_required_input_evidence() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    let action = WorkAction::OutcomeUnknown {
        expected_revision: 0,
        target: target(&facts),
        invocation_id: "first".into(),
        expected_effect_revision: 1,
        reason: "unknown".into(),
    };
    apply(&mut facts, command(action));
    let action = WorkAction::AbandonWork {
        expected_revision: 0,
        expected_control_generation: 0,
        target: target(&facts),
        reason: "explicit new input".into(),
        authorization_ref: "user grant".into(),
    };
    apply(&mut facts, command(action));
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::Abandoned
    );
    assert_eq!(
        facts
            .effects
            .iter()
            .find(|effect| effect.invocation_id == "first")
            .unwrap()
            .status,
        InvocationStatus::OutcomeUnknown
    );
    assert!(facts.head.has_pending_work());
    assert!(facts.head.current_processing_id.is_none());
}

#[test]
fn blocked_reason_resume_keeps_exact_request_and_does_not_repeat_begin() {
    let mut facts = claimed();
    begin_reason(&mut facts);
    let request = facts.processing.as_ref().unwrap().request.clone();
    let action = WorkAction::BlockWork {
        expected_revision: 0,
        target: target(&facts),
        reason: "provider disconnect".into(),
        recovery_condition: "request resume".into(),
    };
    apply(&mut facts, command(action));
    let action = WorkAction::ResumeWork {
        guard: guard(&facts),
        target: target(&facts),
        recovery_evidence: "exact retry authorized".into(),
    };
    apply(&mut facts, command(action));
    assert_eq!(facts.processing.as_ref().unwrap().request, request);
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::ReasonInFlight
    );
    assert_eq!(facts.processing.as_ref().unwrap().budget.reason_requests, 1);
    assert_eq!(facts.processing.as_ref().unwrap().budget.recoveries, 1);
}

#[test]
fn finish_admission_is_exact_even_after_lifecycle_changed() {
    let mut facts = facts();
    admit(&mut facts);
    let admission = facts.admission.as_ref().unwrap().admission.clone();
    facts.control.lifecycle = 2;
    let action = WorkAction::FinishAdmission {
        admission,
        evidence_id: "sdk-stopped".into(),
    };
    apply(&mut facts, command(action));
    assert_eq!(
        facts
            .admission
            .as_ref()
            .unwrap()
            .leaving_evidence_id
            .as_deref(),
        Some("sdk-stopped")
    );
    assert!(facts.head.current_admission_id.is_none());
}

#[test]
fn legacy_unknown_blocks_execution_and_never_becomes_satisfied() {
    let mut facts = claimed();
    apply(
        &mut facts,
        command(WorkAction::QuarantineLegacy {
            expected_revision: 0,
            record_id: "unclassified".into(),
            evidence: "original evidence".into(),
        }),
    );
    let request = payload_ref("request");
    assert_rejected(
        &facts,
        WorkAction::BeginReason {
            guard: guard(&facts),
            target: target(&facts),
            request_id: "request".into(),
            request: ReasonRequest {
                request_digest: request.sha256.clone(),
                payload: request,
                model_ref: "model".into(),
                authorization_ref: "grant".into(),
            },
        },
        WorkRejection::LegacyUnknown,
    );
    assert_eq!(facts.head.legacy_unknown, 1);
    assert_eq!(facts.deliveries[0].obligation, ObligationStatus::InProgress);
}

#[test]
fn typed_queries_are_bounded_and_never_expose_session_state() {
    let query = WorkQuery::new(
        "session",
        WorkSelector::Processing {
            processing_id: "processing".into(),
        },
    );
    query.validate().unwrap();
    let mut oversized = query.clone();
    oversized.limit = MAX_WORK_PAGE_SIZE + 1;
    assert!(oversized.validate().is_err());
    let facts = facts();
    let inspection = WorkInspection {
        session_id: facts.session_id,
        control: facts.control,
        head: facts.head,
        page: WorkPage::Head,
        next_cursor: None,
    };
    let encoded = serde_json::to_value(inspection).unwrap();
    assert!(encoded.get("state").is_none());
    assert!(encoded["head"].get("deliveries").is_none());
}

#[test]
fn paused_control_cannot_cross_dispatch_barrier() {
    let mut facts = act_ready();
    facts.control.status = ControlStatus::Paused;
    assert_rejected(
        &facts,
        WorkAction::BeginDispatch {
            guard: guard(&facts),
            target: target(&facts),
            invocation_id: "first".into(),
            expected_effect_revision: 0,
        },
        WorkRejection::InvalidTransition,
    );
}
