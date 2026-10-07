use super::transition::{guard, next, write_head};
use super::*;
use crate::session::{ExecutionBinding, MessageDisposition, MessageRequirement};
use crate::session_resources::ControlStatus;

#[cfg(test)]
#[path = "mailbox_restart_test.rs"]
mod restart_tests;

pub(super) fn apply(
    command: &WorkCommand,
    facts: &WorkFacts,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    match &command.action {
        WorkAction::StageUserInput {
            input_id,
            content,
            command_id,
            fingerprint,
        } => {
            if input_id.is_empty()
                || command_id.is_empty()
                || content.validate().is_err()
                || matches!(
                    facts.control.status,
                    ControlStatus::Closing | ControlStatus::Closed
                )
            {
                return Err(WorkRejection::InvalidTransition);
            }
            let prior = facts
                .drafts
                .iter()
                .find(|draft| draft.input_id == *input_id);
            if let Some(prior) = prior {
                if prior.command_id == *command_id
                    && prior.fingerprint == *fingerprint
                    && prior.content == *content
                {
                    return Ok(());
                }
                let disposed = prior.publication_id.as_ref().is_some_and(|publication_id| {
                    facts.deliveries.iter().any(|delivery| {
                        delivery.delivery_id == *publication_id
                            && delivery.recipient_lifecycle == prior.recipient_lifecycle
                            && delivery.obligation == ObligationStatus::Abandoned
                            && delivery.disposition.is_some()
                    })
                });
                if prior.command_id == *command_id
                    || (prior.status != StagedUserInputStatus::Withdrawn && !disposed)
                {
                    return Err(WorkRejection::Conflict);
                }
            }
            let mut head = facts.head.clone();
            if let Some(prior) = prior {
                head.publication_generation = next(
                    head.publication_generation
                        .max(prior.publication_generation),
                )?;
            }
            let sequence = head.next_delivery_seq;
            head.next_delivery_seq = next(sequence)?;
            writes.push(WorkWrite::Draft {
                expected_revision: prior.map(|draft| draft.revision),
                record: StagedUserInput {
                    input_id: input_id.clone(),
                    recipient_lifecycle: command.recipient_lifecycle,
                    revision: match prior {
                        Some(draft) => next(draft.revision)?,
                        None => 0,
                    },
                    sequence,
                    publication_generation: head.publication_generation,
                    content: content.clone(),
                    command_id: command_id.clone(),
                    fingerprint: *fingerprint,
                    status: StagedUserInputStatus::Queued,
                    publication_id: None,
                    withdrawal: None,
                },
            });
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::WithdrawStagedUserInput {
            input_id,
            command_id,
            fingerprint,
        } => {
            let prior = facts
                .drafts
                .iter()
                .find(|draft| draft.input_id == *input_id)
                .ok_or(WorkRejection::Conflict)?;
            if command_id.is_empty() {
                return Err(WorkRejection::InvalidTransition);
            }
            if prior.status == StagedUserInputStatus::Withdrawn {
                return if prior.withdrawal.as_ref() == Some(&(command_id.clone(), *fingerprint)) {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            if prior.status != StagedUserInputStatus::Queued {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut draft = prior.clone();
            draft.revision = next(draft.revision)?;
            draft.status = StagedUserInputStatus::Withdrawn;
            draft.withdrawal = Some((command_id.clone(), *fingerprint));
            writes.push(WorkWrite::Draft {
                expected_revision: Some(prior.revision),
                record: draft,
            });
            Ok(())
        }
        WorkAction::PublishDelivery { delivery } => {
            let mut head = facts.head.clone();
            accept(command, facts, delivery, &mut head, receipt, writes)?;
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::PublishTaskSettlement { delivery, binding } => {
            let effect = facts
                .effects
                .iter()
                .find(|effect| effect.invocation_id == binding.invocation_id)
                .ok_or(WorkRejection::Conflict)?;
            if effect.binding.as_ref() != Some(binding)
                || !matches!(
                    effect.status,
                    InvocationStatus::DispatchAccepted
                        | InvocationStatus::OutcomeUnknown
                        | InvocationStatus::Settled
                )
                || effect.recipient_lifecycle != binding.recipient_lifecycle
            {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut head = facts.head.clone();
            accept(command, facts, delivery, &mut head, receipt, writes)?;
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::PublishStagedUserInputs {
            expected_control_generation,
            expected_attempt,
            interrupt_current,
            deliveries,
            ..
        } => {
            if facts.control.control_generation != *expected_control_generation {
                return Err(WorkRejection::StaleControlGeneration);
            }
            if facts.control.attempt != *expected_attempt {
                return Err(WorkRejection::StaleExecution);
            }
            if deliveries.is_empty()
                || deliveries.len() > MAX_WORK_PAGE_SIZE as usize
                || matches!(
                    facts.control.status,
                    ControlStatus::Closing | ControlStatus::Closed
                )
            {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut selected = Vec::new();
            let mut head = facts.head.clone();
            head.publication_generation = next(head.publication_generation)?;
            for delivery in deliveries {
                let identity: UserInputPublicationIdentity = serde_json::from_str(
                    delivery
                        .event
                        .causation_id
                        .as_deref()
                        .ok_or(WorkRejection::InvalidTransition)?,
                )
                .map_err(|_| WorkRejection::InvalidTransition)?;
                if identity.input_id.is_empty()
                    || identity.command_id.is_empty()
                    || identity.publication_generation.is_empty()
                {
                    return Err(WorkRejection::InvalidTransition);
                }
                let draft = facts
                    .drafts
                    .iter()
                    .find(|draft| draft.input_id == identity.input_id)
                    .ok_or(WorkRejection::Conflict)?;
                let binding = &identity.draft_binding;
                let content_matches = binding.draft_revision == draft.revision
                    && binding.draft_fingerprint == draft.fingerprint
                    && binding.canonical_content == delivery.event.content.content;
                if draft.status != StagedUserInputStatus::Queued
                    || draft.recipient_lifecycle != command.recipient_lifecycle
                    || selected.contains(&draft.input_id)
                    || delivery.purpose != DeliveryPurpose::UserInput
                    || delivery.event.content.role != "user"
                    || !content_matches
                    || facts
                        .deliveries
                        .iter()
                        .any(|prior| prior.delivery_id == delivery.delivery_id)
                {
                    return Err(WorkRejection::Conflict);
                }
                selected.push(draft.input_id.clone());
                accept(command, facts, delivery, &mut head, receipt, writes)?;
                let mut updated = draft.clone();
                updated.revision = next(updated.revision)?;
                updated.status = StagedUserInputStatus::Published;
                updated.publication_generation = head.publication_generation;
                updated.publication_id = Some(delivery.delivery_id.clone());
                writes.push(WorkWrite::Draft {
                    expected_revision: Some(draft.revision),
                    record: updated,
                });
            }
            if *interrupt_current {
                if let Some(processing) = &facts.processing {
                    if head.current_processing_id.as_ref() != Some(&processing.processing_id) {
                        return Err(WorkRejection::Conflict);
                    }
                    if !matches!(processing.stage, WorkStage::Settled | WorkStage::Abandoned) {
                        let mut abandoned = processing.clone();
                        abandoned.stage = WorkStage::Abandoned;
                        abandoned.blocked_evidence = Some(command.mutation_id.clone());
                        super::processing::abandon_processing_inputs(
                            facts, &abandoned, &mut head, writes,
                        )?;
                        super::transition::write_processing(abandoned, receipt, writes)?;
                    }
                } else if head.current_processing_id.is_some() {
                    return Err(WorkRejection::Conflict);
                }
                head.current_processing_id = None;
                let mut control = facts.control.clone();
                control.revision = next(control.revision)?;
                control.control_generation = next(control.control_generation)?;
                control.attempt = None;
                control.status = ControlStatus::Active;
                writes.push(WorkWrite::Control {
                    expected_revision: facts.control.revision,
                    record: control,
                });
            } else if facts.control.status != ControlStatus::Active {
                return Err(WorkRejection::InvalidTransition);
            }
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::ClaimBatch {
            guard: execution_guard,
            batch_id,
            delivery_ids,
        } => {
            guard(facts, execution_guard)?;
            if batch_id.is_empty()
                || delivery_ids.is_empty()
                || delivery_ids.len() as u64 > facts.head.limits.max_batch_size
                || facts.processing.is_some()
                || facts.head.current_processing_id.is_some()
            {
                return Err(WorkRejection::InvalidTransition);
            }
            let admission = facts
                .admission
                .as_ref()
                .ok_or(WorkRejection::StaleExecution)?;
            if admission.leaving_evidence_id.is_some()
                || admission.admission.execution != execution_guard.execution
                || admission.admission.session_id != command.session_id
                || admission.admission.lifecycle != command.recipient_lifecycle
                || admission.admission.work_id != *batch_id
                || admission.admission.work_revision != 0
                || facts.head.current_admission_id.as_ref()
                    != Some(&admission.admission.admission_id)
            {
                return Err(WorkRejection::StaleExecution);
            }
            let initial_delivery_ids = admission
                .initial_delivery_ids
                .as_ref()
                .filter(|identities| !identities.is_empty())
                .ok_or(WorkRejection::LegacyUnknown)?;
            if initial_delivery_ids != delivery_ids {
                return Err(WorkRejection::Conflict);
            }
            let execution = ExecutionBinding {
                turn_id: execution_guard.execution.turn_id,
                attempt_id: execution_guard.execution.attempt_id.clone(),
            };
            let delegation = facts
                .deliveries
                .iter()
                .find(|delivery| delivery.delivery_id == delivery_ids[0])
                .ok_or(WorkRejection::Conflict)?
                .delegation
                .clone();
            let mut seen = Vec::new();
            let mut participates = false;
            let mut head = facts.head.clone();
            for (ordinal, delivery_id) in delivery_ids.iter().enumerate() {
                if seen.contains(delivery_id) {
                    return Err(WorkRejection::Conflict);
                }
                seen.push(delivery_id.clone());
                let prior = facts
                    .deliveries
                    .iter()
                    .find(|delivery| delivery.delivery_id == *delivery_id)
                    .ok_or(WorkRejection::Conflict)?;
                if prior.processing_id.is_some()
                    || prior.disposition.is_some()
                    || prior.recipient_lifecycle != command.recipient_lifecycle
                {
                    return Err(WorkRejection::InvalidTransition);
                }
                if prior.delegation != delegation {
                    return Err(WorkRejection::Conflict);
                }
                let mut delivery = prior.clone();
                delivery.revision = next(delivery.revision)?;
                delivery.projection_version = next(delivery.projection_version)?;
                delivery.processing_id = Some(batch_id.clone());
                delivery.batch_ordinal = Some(ordinal as u32);
                delivery.delegation = None;
                match delivery.publication.policy.disposition(&execution) {
                    MessageDisposition::Process => {
                        participates = true;
                        delivery.participates_in_reason = true;
                        delivery.obligation = ObligationStatus::InProgress;
                        delivery.projection = Some(delivery.publication.event.content.message_id);
                        writes.push(WorkWrite::Transcript {
                            payload: delivery.publication.event.content.clone(),
                        });
                    }
                    MessageDisposition::ProjectOnly => {
                        delivery.obligation = ObligationStatus::Satisfied;
                        release(&mut head, &delivery)?;
                        delivery.projection = Some(delivery.publication.event.content.message_id);
                        writes.push(WorkWrite::Transcript {
                            payload: delivery.publication.event.content.clone(),
                        });
                    }
                    MessageDisposition::Suppressed => {
                        delivery.obligation = ObligationStatus::Suppressed;
                        delivery.disposition = Some("execution-suppressed".into());
                        release(&mut head, &delivery)?;
                    }
                }
                writes.push(WorkWrite::Delivery {
                    expected_revision: Some(prior.revision),
                    record: delivery,
                });
            }
            let stage = if participates {
                WorkStage::ReasonReady
            } else {
                WorkStage::Settled
            };
            let processing = Processing { processing_id: batch_id.clone(), recipient_lifecycle: command.recipient_lifecycle,
                revision: 0, execution: execution_guard.execution.clone(), stage, phase_sequence: 0,
                delivery_count: delivery_ids.len() as u32,
                reason_delivery_count: writes.iter().filter(|write| matches!(write, WorkWrite::Delivery { record, .. } if record.participates_in_reason)).count() as u32,
                budget: WorkBudget::default(), checkpoint: Some(command.mutation_id.clone()), request_id: None,
                request: None, response: None, remaining_effects: 0, resume_stage: None, blocked_evidence: None,
                recovery_condition: None, delegation };
            if participates {
                head.current_processing_id = Some(batch_id.clone());
            }
            receipt.work_id = Some(batch_id.clone());
            receipt.work_revision = Some(0);
            receipt.stage = Some(stage);
            receipt.batch_id = Some(batch_id.clone());
            writes.push(WorkWrite::Processing {
                expected_revision: None,
                record: processing,
            });
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::WithdrawDelivery {
            expected_control_generation,
            delivery_id,
            authorization_ref,
            ..
        } => {
            if authorization_ref.is_empty() {
                return Err(WorkRejection::InvalidTransition);
            }
            if expected_control_generation
                .is_some_and(|expected| expected != facts.control.control_generation)
            {
                return Err(WorkRejection::StaleControlGeneration);
            }
            let delivery = facts
                .deliveries
                .iter()
                .find(|delivery| delivery.delivery_id == *delivery_id)
                .ok_or(WorkRejection::Conflict)?;
            if delivery.processing_id.is_some() || delivery.projection.is_some() {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut associated = facts.drafts.iter().filter(|draft| {
                draft.publication_id.as_ref() == Some(delivery_id)
                    && draft.recipient_lifecycle == delivery.recipient_lifecycle
            });
            let prior = associated.next().ok_or(WorkRejection::Conflict)?;
            if associated.next().is_some() || prior.status != StagedUserInputStatus::Published {
                return Err(WorkRejection::Conflict);
            }
            let mut draft = prior.clone();
            draft.revision = next(draft.revision)?;
            draft.status = if expected_control_generation.is_some() {
                StagedUserInputStatus::Queued
            } else {
                StagedUserInputStatus::Withdrawn
            };
            draft.publication_id = None;
            draft.withdrawal = None;
            dispose(
                facts,
                delivery_id,
                "withdrawn",
                Some(authorization_ref),
                writes,
            )?;
            writes.push(WorkWrite::Draft {
                expected_revision: Some(prior.revision),
                record: draft,
            });
            Ok(())
        }
        WorkAction::AbandonDelivery {
            guard: execution_guard,
            delivery_id,
            reason,
            evidence,
        } => {
            guard(facts, execution_guard)?;
            if reason.is_empty() || evidence.is_empty() {
                return Err(WorkRejection::InvalidTransition);
            }
            dispose(facts, delivery_id, reason, Some(evidence), writes)
        }
        _ => Err(WorkRejection::InvalidTransition),
    }
}

fn accept(
    command: &WorkCommand,
    facts: &WorkFacts,
    publication: &PublishDelivery,
    head: &mut SessionWorkHead,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    if publication.delivery_id.is_empty()
        || publication.event.event_id.is_empty()
        || publication.event.producer_namespace.is_empty()
        || publication.event.content.validate().is_err()
    {
        return Err(WorkRejection::InvalidTransition);
    }
    if let Some(prior) = facts
        .deliveries
        .iter()
        .find(|delivery| delivery.delivery_id == publication.delivery_id)
    {
        if prior.publication != *publication {
            return Err(WorkRejection::Conflict);
        }
        receipt.delivery_id = Some(prior.delivery_id.clone());
        receipt.admission_sequence = Some(prior.admission_sequence);
        return Ok(());
    }
    if writes.iter().any(|write| matches!(write, WorkWrite::Delivery { record, .. } if record.delivery_id == publication.delivery_id)) {
        return Err(WorkRejection::Conflict);
    }
    let required = publication.policy.requirement == MessageRequirement::Required;
    let byte_length = publication.event.content.content.byte_length;
    let (count, bytes, count_limit, byte_limit) = if required {
        (
            &mut head.required_count,
            &mut head.required_bytes,
            head.limits.required_deliveries,
            head.limits.required_bytes,
        )
    } else {
        (
            &mut head.optional_count,
            &mut head.optional_bytes,
            head.limits.optional_deliveries,
            head.limits.optional_bytes,
        )
    };
    *count = next(*count)?;
    *bytes = bytes
        .checked_add(byte_length)
        .ok_or(WorkRejection::VersionExhausted)?;
    if *count > count_limit || *bytes > byte_limit {
        return Err(WorkRejection::Capacity);
    }
    let sequence = head.next_delivery_seq;
    head.next_delivery_seq = next(sequence)?;
    receipt.delivery_id = Some(publication.delivery_id.clone());
    receipt.admission_sequence = Some(sequence);
    writes.push(WorkWrite::Delivery {
        expected_revision: None,
        record: Delivery {
            delivery_id: publication.delivery_id.clone(),
            recipient_lifecycle: command.recipient_lifecycle,
            revision: 0,
            publication: publication.clone(),
            admission_sequence: sequence,
            projection: None,
            projection_version: 0,
            processing_id: None,
            batch_ordinal: None,
            participates_in_reason: false,
            obligation: ObligationStatus::Pending,
            disposition: None,
            delegation: None,
        },
    });
    Ok(())
}

pub(super) fn release(
    head: &mut SessionWorkHead,
    delivery: &Delivery,
) -> Result<(), WorkRejection> {
    let (count, bytes) = if delivery.publication.policy.requirement == MessageRequirement::Required
    {
        (&mut head.required_count, &mut head.required_bytes)
    } else {
        (&mut head.optional_count, &mut head.optional_bytes)
    };
    *count = count.checked_sub(1).ok_or(WorkRejection::Conflict)?;
    *bytes = bytes
        .checked_sub(delivery.publication.event.content.content.byte_length)
        .ok_or(WorkRejection::Conflict)?;
    Ok(())
}

fn dispose(
    facts: &WorkFacts,
    delivery_id: &str,
    reason: &str,
    evidence: Option<&String>,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    let prior = facts
        .deliveries
        .iter()
        .find(|delivery| delivery.delivery_id == delivery_id)
        .ok_or(WorkRejection::Conflict)?;
    if prior.processing_id.is_some() && evidence.is_none() {
        return Err(WorkRejection::InvalidTransition);
    }
    if !matches!(
        prior.obligation,
        ObligationStatus::Pending | ObligationStatus::Blocked
    ) {
        return Err(WorkRejection::InvalidTransition);
    }
    let mut delivery = prior.clone();
    delivery.revision = next(delivery.revision)?;
    delivery.obligation = ObligationStatus::Abandoned;
    delivery.disposition = Some(match evidence {
        Some(value) => format!("{reason}: {value}"),
        None => reason.into(),
    });
    let mut head = facts.head.clone();
    release(&mut head, &delivery)?;
    writes.push(WorkWrite::Delivery {
        expected_revision: Some(prior.revision),
        record: delivery,
    });
    write_head(facts, head, writes);
    Ok(())
}
