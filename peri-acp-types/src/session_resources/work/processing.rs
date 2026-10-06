use super::transition::{guard, next, target, write_head, write_processing};
use super::*;
use crate::session_resources::ControlStatus;

pub(super) fn apply(
    command: &WorkCommand,
    facts: &WorkFacts,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    match &command.action {
        WorkAction::RegisterAdmission { admission } => {
            if !validate_execution_association(admission, &facts.control)
                || admission.session_id != command.session_id
                || facts.control.status != ControlStatus::Active
                || facts.head.legacy_unknown != 0
            {
                return Err(WorkRejection::StaleExecution);
            }
            if let Some(prior) = &facts.admission {
                return if prior.admission == *admission && prior.leaving_evidence_id.is_none() {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            if let Some(processing) = &facts.processing {
                if processing.processing_id != admission.work_id
                    || processing.revision != admission.work_revision
                    || matches!(processing.stage, WorkStage::Settled | WorkStage::Abandoned)
                {
                    return Err(WorkRejection::StaleWorkRevision);
                }
            }
            if facts.head.current_admission_id.is_some() {
                return Err(WorkRejection::Conflict);
            }
            if facts.control.attempt.is_none() {
                let mut control = facts.control.clone();
                control.revision = next(control.revision)?;
                control.attempt = Some(admission.execution.clone());
                writes.push(WorkWrite::Control {
                    expected_revision: facts.control.revision,
                    record: control,
                });
            }
            let mut head = facts.head.clone();
            head.current_admission_id = Some(admission.admission_id.clone());
            writes.push(WorkWrite::Admission {
                expected_entering_mutation_id: None,
                record: AdmissionRecord {
                    admission: admission.clone(),
                    entering_mutation_id: command.mutation_id.clone(),
                    leaving_evidence_id: None,
                },
            });
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::FinishAdmission {
            admission,
            evidence_id,
        } => {
            let prior = facts.admission.as_ref().ok_or(WorkRejection::Conflict)?;
            if admission.lifecycle != command.recipient_lifecycle
                || prior.admission != *admission
                || evidence_id.is_empty()
                || admission.session_id != command.session_id
            {
                return Err(WorkRejection::Conflict);
            }
            if let Some(evidence) = &prior.leaving_evidence_id {
                return if evidence == evidence_id {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            let mut record = prior.clone();
            record.leaving_evidence_id = Some(evidence_id.clone());
            writes.push(WorkWrite::Admission {
                expected_entering_mutation_id: Some(prior.entering_mutation_id.clone()),
                record,
            });
            let mut head = facts.head.clone();
            if head.current_admission_id.as_ref() == Some(&admission.admission_id) {
                head.current_admission_id = None;
                if facts.control.attempt.as_ref() == Some(&admission.execution) {
                    let mut control = facts.control.clone();
                    control.revision = next(control.revision)?;
                    control.attempt = None;
                    writes.push(WorkWrite::Control {
                        expected_revision: facts.control.revision,
                        record: control,
                    });
                }
            }
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::BindResourceOwners {
            connections_json,
            authorization_ref,
            ..
        } => {
            if authorization_ref.is_empty()
                || serde_json::from_str::<serde_json::Value>(connections_json).is_err()
            {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut descriptor = descriptor(command, facts);
            let owners = ResourceOwnerBinding {
                recipient_lifecycle: command.recipient_lifecycle,
                connections_json: connections_json.clone(),
                authorization_ref: authorization_ref.clone(),
            };
            if descriptor
                .resource_owners
                .as_ref()
                .is_some_and(|prior| prior != &owners)
            {
                return Err(WorkRejection::Conflict);
            }
            descriptor.resource_owners = Some(owners);
            write_descriptor(facts, descriptor, writes)
        }
        WorkAction::BindChildResumeMetadata { metadata_json, .. } => {
            if serde_json::from_str::<serde_json::Value>(metadata_json).is_err() {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut descriptor = descriptor(command, facts);
            if descriptor
                .child_resume_metadata_json
                .as_ref()
                .is_some_and(|prior| prior != metadata_json)
            {
                return Err(WorkRejection::Conflict);
            }
            descriptor.child_resume_metadata_json = Some(metadata_json.clone());
            write_descriptor(facts, descriptor, writes)
        }
        WorkAction::BindWorkDelegation {
            work_id,
            binding,
            parent_binding_receipt,
            ..
        } => {
            let mut processing = facts.processing.clone().ok_or(WorkRejection::Conflict)?;
            if processing.processing_id != *work_id
                || binding.recipient_lifecycle == 0
                || parent_binding_receipt.decision != WorkDecision::Accepted
                || parent_binding_receipt.session_id != binding.initiator_session_id
                || facts.parent_binding_receipt.as_ref() != Some(parent_binding_receipt)
                || !facts.parent_effect.as_ref().is_some_and(|effect| {
                    effect.invocation_id == binding.invocation_id
                        && effect.recipient_lifecycle == binding.recipient_lifecycle
                        && effect.binding.as_ref() == Some(binding)
                })
                || binding.invocation_id.is_empty()
                || binding.owner_task_id.is_empty()
            {
                return Err(WorkRejection::Conflict);
            }
            let delegation = DelegationRef {
                parent_session_id: binding.initiator_session_id.clone(),
                parent_lifecycle: binding.recipient_lifecycle,
                delegation_id: binding.invocation_id.clone(),
            };
            if processing
                .delegation
                .as_ref()
                .is_some_and(|prior| prior != &delegation)
            {
                return Err(WorkRejection::Conflict);
            }
            processing.delegation = Some(delegation);
            write_processing(processing, receipt, writes)
        }
        WorkAction::BeginReason {
            guard: execution_guard,
            target: work_target,
            request_id,
            request,
        } => {
            guard(facts, execution_guard)?;
            let mut processing = target(facts, work_target)?;
            if processing.stage != WorkStage::ReasonReady
                || request_id.is_empty()
                || request.payload.validate().is_err()
                || request.request_digest != request.payload.sha256
                || request.model_ref.is_empty()
                || request.authorization_ref.is_empty()
            {
                return Err(WorkRejection::InvalidTransition);
            }
            processing.budget.reason_requests = next(processing.budget.reason_requests)?;
            if processing.budget.reason_requests > facts.head.limits.reason_requests {
                return Err(WorkRejection::Capacity);
            }
            processing.execution = execution_guard.execution.clone();
            processing.stage = WorkStage::ReasonInFlight;
            processing.request_id = Some(request_id.clone());
            processing.request = Some(request.payload.clone());
            processing.checkpoint = Some(command.mutation_id.clone());
            write_processing(processing, receipt, writes)
        }
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: execution_guard,
            target: work_target,
            request_id,
            response,
            dispatch_intents,
            next_work_id,
        } => {
            guard(facts, execution_guard)?;
            let mut processing = target(facts, work_target)?;
            if processing.stage != WorkStage::ReasonInFlight
                || processing.request_id.as_ref() != Some(request_id)
                || response.validate().is_err()
                || response.role != "assistant"
                || dispatch_intents.len() > MAX_WORK_PAGE_SIZE as usize
                || next_work_id
                    .as_ref()
                    .is_some_and(|identity| identity != &processing.processing_id)
            {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut unique = Vec::new();
            let mut tool_calls = Vec::new();
            for intent in dispatch_intents {
                super::effect::validate_intent(intent)?;
                if unique.contains(&intent.invocation_id)
                    || tool_calls.contains(&intent.tool_call_id)
                    || facts
                        .effects
                        .iter()
                        .any(|effect| effect.invocation_id == intent.invocation_id)
                {
                    return Err(WorkRejection::Conflict);
                }
                unique.push(intent.invocation_id.clone());
                tool_calls.push(intent.tool_call_id.clone());
                writes.push(WorkWrite::Effect {
                    expected_revision: None,
                    record: Effect {
                        invocation_id: intent.invocation_id.clone(),
                        recipient_lifecycle: command.recipient_lifecycle,
                        revision: 0,
                        processing_id: Some(processing.processing_id.clone()),
                        phase_sequence: processing.phase_sequence,
                        intent: intent.clone(),
                        binding: None,
                        status: InvocationStatus::Prepared,
                        outcome: None,
                        unknown_reason: None,
                        delegation: None,
                    },
                });
            }
            let mut head = facts.head.clone();
            let mut satisfied = 0u32;
            for prior in &facts.deliveries {
                if prior.processing_id.as_ref() != Some(&processing.processing_id)
                    || !prior.participates_in_reason
                    || prior.obligation != ObligationStatus::InProgress
                {
                    continue;
                }
                let mut delivery = prior.clone();
                delivery.revision = next(delivery.revision)?;
                delivery.obligation = ObligationStatus::Satisfied;
                super::mailbox::release(&mut head, &delivery)?;
                satisfied += 1;
                writes.push(WorkWrite::Delivery {
                    expected_revision: Some(prior.revision),
                    record: delivery,
                });
            }
            if processing.phase_sequence == 0 && satisfied != processing.reason_delivery_count {
                return Err(WorkRejection::Conflict);
            }
            head.unresolved_effects = head
                .unresolved_effects
                .checked_add(dispatch_intents.len() as u64)
                .ok_or(WorkRejection::VersionExhausted)?;
            processing.remaining_effects = dispatch_intents.len() as u64;
            processing.response = Some(response.clone());
            processing.checkpoint = Some(command.mutation_id.clone());
            writes.push(WorkWrite::Transcript {
                payload: response.clone(),
            });
            if dispatch_intents.is_empty() {
                processing.stage = WorkStage::Settled;
                if head.current_processing_id.as_ref() == Some(&processing.processing_id) {
                    head.current_processing_id = None;
                }
            } else {
                processing.stage = WorkStage::ActReady;
            }
            write_head(facts, head, writes);
            write_processing(processing, receipt, writes)
        }
        WorkAction::BlockWork {
            target: work_target,
            reason,
            recovery_condition,
            ..
        } => {
            let mut processing = target(facts, work_target)?;
            if reason.is_empty()
                || recovery_condition.is_empty()
                || matches!(
                    processing.stage,
                    WorkStage::Settled | WorkStage::Abandoned | WorkStage::Blocked
                )
            {
                return Err(WorkRejection::InvalidTransition);
            }
            processing.resume_stage = Some(processing.stage);
            processing.stage = WorkStage::Blocked;
            processing.blocked_evidence = Some(reason.clone());
            processing.recovery_condition = Some(recovery_condition.clone());
            write_processing(processing, receipt, writes)
        }
        WorkAction::ResumeWork {
            guard: execution_guard,
            target: work_target,
            recovery_evidence,
        } => {
            guard(facts, execution_guard)?;
            let mut processing = target(facts, work_target)?;
            if processing.stage != WorkStage::Blocked || recovery_evidence.is_empty() {
                return Err(WorkRejection::InvalidTransition);
            }
            processing.budget.recoveries = next(processing.budget.recoveries)?;
            if processing.budget.recoveries > facts.head.limits.recoveries {
                return Err(WorkRejection::Capacity);
            }
            processing.stage = processing
                .resume_stage
                .take()
                .ok_or(WorkRejection::InvalidTransition)?;
            if !matches!(
                processing.stage,
                WorkStage::ReasonReady | WorkStage::ReasonInFlight | WorkStage::ActReady
            ) {
                return Err(WorkRejection::InvalidTransition);
            }
            if processing.stage == WorkStage::ReasonInFlight && processing.request.is_none() {
                return Err(WorkRejection::InvalidTransition);
            }
            processing.execution = execution_guard.execution.clone();
            processing.blocked_evidence = Some(recovery_evidence.clone());
            write_processing(processing, receipt, writes)
        }
        WorkAction::AbandonWork {
            expected_control_generation,
            target: work_target,
            reason,
            authorization_ref,
            ..
        } => {
            if *expected_control_generation != facts.control.control_generation {
                return Err(WorkRejection::StaleControlGeneration);
            }
            if reason.is_empty() || authorization_ref.is_empty() {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut processing = target(facts, work_target)?;
            if matches!(processing.stage, WorkStage::Settled | WorkStage::Abandoned) {
                return Err(WorkRejection::InvalidTransition);
            }
            processing.stage = WorkStage::Abandoned;
            processing.blocked_evidence = Some(reason.clone());
            processing.checkpoint = Some(command.mutation_id.clone());
            let mut head = facts.head.clone();
            if head.current_processing_id.as_ref() == Some(&processing.processing_id) {
                head.current_processing_id = None;
            }
            write_head(facts, head, writes);
            write_processing(processing, receipt, writes)
        }
        WorkAction::SettleWork {
            target: work_target,
            ..
        } => {
            let mut processing = target(facts, work_target)?;
            if processing.remaining_effects != 0 || processing.stage != WorkStage::ReasonReady {
                return Err(WorkRejection::InvalidTransition);
            }
            processing.stage = WorkStage::Settled;
            let mut head = facts.head.clone();
            if head.current_processing_id.as_ref() == Some(&processing.processing_id) {
                head.current_processing_id = None;
            }
            write_head(facts, head, writes);
            write_processing(processing, receipt, writes)
        }
        WorkAction::ResetBudget {
            budget_id,
            authorization_ref,
            ..
        } => {
            let mut processing = facts.processing.clone().ok_or(WorkRejection::Conflict)?;
            if authorization_ref.is_empty() || *budget_id != processing.processing_id {
                return Err(WorkRejection::InvalidTransition);
            }
            processing.budget = WorkBudget::default();
            write_processing(processing, receipt, writes)
        }
        WorkAction::QuarantineLegacy {
            record_id,
            evidence,
            ..
        } => {
            if record_id.is_empty() || evidence.is_empty() {
                return Err(WorkRejection::InvalidTransition);
            }
            let record = LegacyEvidence {
                record_id: record_id.clone(),
                evidence: evidence.clone(),
            };
            if let Some(prior) = &facts.legacy_evidence {
                return if prior == &record {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            let mut head = facts.head.clone();
            head.legacy_unknown = next(head.legacy_unknown)?;
            writes.push(WorkWrite::LegacyEvidence { record });
            write_head(facts, head, writes);
            Ok(())
        }
        _ => Err(WorkRejection::InvalidTransition),
    }
}

fn descriptor(command: &WorkCommand, facts: &WorkFacts) -> RecoveryDescriptor {
    facts
        .recovery_descriptor
        .clone()
        .unwrap_or_else(|| RecoveryDescriptor {
            descriptor_id: format!("{}:{}", command.session_id, command.recipient_lifecycle),
            recipient_lifecycle: command.recipient_lifecycle,
            revision: 0,
            resource_owners: None,
            child_resume_metadata_json: None,
        })
}

fn write_descriptor(
    facts: &WorkFacts,
    mut descriptor: RecoveryDescriptor,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    let expected_revision = facts
        .recovery_descriptor
        .as_ref()
        .map(|record| record.revision);
    if let Some(previous) = expected_revision {
        descriptor.revision = next(previous)?;
    }
    let mut head = facts.head.clone();
    head.recovery_descriptor_id = Some(descriptor.descriptor_id.clone());
    writes.push(WorkWrite::RecoveryDescriptor {
        expected_revision,
        record: descriptor,
    });
    write_head(facts, head, writes);
    Ok(())
}
