use super::transition::{guard, next, write_head, write_processing};
use super::*;

pub(super) fn apply(
    command: &WorkCommand,
    facts: &WorkFacts,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    match &command.action {
        WorkAction::PrepareInvocation { intent, .. } => {
            validate_intent(intent)?;
            if let Some(prior) = facts
                .effects
                .iter()
                .find(|effect| effect.invocation_id == intent.invocation_id)
            {
                return if prior.intent == *intent
                    && prior.recipient_lifecycle == command.recipient_lifecycle
                {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            writes.push(WorkWrite::Effect {
                expected_revision: None,
                record: Effect {
                    invocation_id: intent.invocation_id.clone(),
                    recipient_lifecycle: command.recipient_lifecycle,
                    revision: 0,
                    processing_id: None,
                    phase_sequence: 0,
                    intent: intent.clone(),
                    binding: None,
                    status: InvocationStatus::Prepared,
                    outcome: None,
                    unknown_reason: None,
                    delegation: None,
                },
            });
            let mut head = facts.head.clone();
            head.unresolved_effects = next(head.unresolved_effects)?;
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::BeginDispatch {
            guard: execution_guard,
            target: work_target,
            invocation_id,
            expected_effect_revision,
        } => {
            guard(facts, execution_guard)?;
            let prior = effect(facts, invocation_id, *expected_effect_revision)?;
            if prior.status != InvocationStatus::Prepared
                || prior.recipient_lifecycle != command.recipient_lifecycle
            {
                return Err(WorkRejection::InvalidTransition);
            }
            if let Some(processing_id) = &prior.processing_id {
                let mut processing = processing_for_effect(facts, work_target, prior)?;
                if processing.stage != WorkStage::ActReady
                    || processing.processing_id != *processing_id
                {
                    return Err(WorkRejection::InvalidTransition);
                }
                if processing.budget.dispatches >= facts.head.limits.dispatches {
                    return super::processing::block_budget(
                        processing,
                        crate::error::WorkBudgetKind::Dispatches,
                        receipt,
                        writes,
                    );
                }
                processing.budget.dispatches = next(processing.budget.dispatches)?;
                write_processing(processing, receipt, writes)?;
            }
            let mut updated = prior.clone();
            updated.status = InvocationStatus::DispatchAccepted;
            write_effect(updated, prior.revision, writes)
        }
        WorkAction::OutcomeUnknown {
            target: work_target,
            invocation_id,
            expected_effect_revision,
            reason,
            ..
        } => {
            let prior = effect(facts, invocation_id, *expected_effect_revision)?;
            if prior.recipient_lifecycle != command.recipient_lifecycle
                || prior.status != InvocationStatus::DispatchAccepted
                || reason.is_empty()
            {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut updated = prior.clone();
            updated.status = InvocationStatus::OutcomeUnknown;
            updated.unknown_reason = Some(reason.clone());
            write_effect(updated, prior.revision, writes)?;
            if prior.processing_id.is_some() {
                let mut processing = processing_for_effect(facts, work_target, prior)?;
                if processing.stage == WorkStage::ActReady {
                    processing.resume_stage = Some(WorkStage::ActReady);
                    processing.stage = WorkStage::Blocked;
                    processing.blocked_evidence = Some(reason.clone());
                    processing.recovery_condition = Some(invocation_id.clone());
                    write_processing(processing, receipt, writes)?;
                } else if !matches!(processing.stage, WorkStage::Blocked | WorkStage::Abandoned) {
                    return Err(WorkRejection::InvalidTransition);
                }
            }
            Ok(())
        }
        WorkAction::CommitAct {
            guard: execution_guard,
            target: work_target,
            results,
            next_work_id,
        } => {
            if results.is_empty() || results.len() > MAX_WORK_PAGE_SIZE as usize {
                return Err(WorkRejection::InvalidTransition);
            }
            let mut processing = facts.processing.clone();
            if let Some(record) = &processing {
                if record.processing_id != work_target.work_id
                    || record.revision < work_target.expected_work_revision
                    || !matches!(
                        record.stage,
                        WorkStage::ActReady | WorkStage::Blocked | WorkStage::Abandoned
                    )
                {
                    return Err(WorkRejection::InvalidTransition);
                }
                let admitted = facts.admission.as_ref().is_some_and(|entry| {
                    entry.admission.execution == execution_guard.execution
                        && entry.admission.session_id == command.session_id
                        && entry.admission.lifecycle == command.recipient_lifecycle
                });
                if record.execution != execution_guard.execution && !admitted {
                    return Err(WorkRejection::StaleExecution);
                }
                if next_work_id
                    .as_ref()
                    .is_some_and(|identity| identity != &record.processing_id)
                {
                    return Err(WorkRejection::InvalidTransition);
                }
            }
            let mut seen = Vec::new();
            let mut newly_settled = 0u64;
            let mut related_settled = 0u64;
            for result in results {
                if seen.contains(&result.invocation_id) {
                    return Err(WorkRejection::Conflict);
                }
                seen.push(result.invocation_id.clone());
                let prior = effect(
                    facts,
                    &result.invocation_id,
                    result.expected_effect_revision,
                )?;
                if prior.recipient_lifecycle != command.recipient_lifecycle {
                    return Err(WorkRejection::StaleLifecycle);
                }
                if prior.status == InvocationStatus::Settled {
                    if prior.outcome.as_ref() != Some(&result.outcome) {
                        return Err(WorkRejection::Conflict);
                    }
                    continue;
                }
                if !matches!(
                    prior.status,
                    InvocationStatus::DispatchAccepted | InvocationStatus::OutcomeUnknown
                ) && !(prior.status == InvocationStatus::Prepared
                    && matches!(result.outcome, InvocationOutcome::Cancelled { .. }))
                {
                    return Err(WorkRejection::InvalidTransition);
                }
                if prior.processing_id.is_some() {
                    let record = processing.as_ref().ok_or(WorkRejection::Conflict)?;
                    if prior.processing_id.as_ref() != Some(&record.processing_id)
                        || prior.phase_sequence != record.phase_sequence
                    {
                        return Err(WorkRejection::Conflict);
                    }
                    related_settled = next(related_settled)?;
                }
                match &result.outcome {
                    InvocationOutcome::Completed { result: payload }
                    | InvocationOutcome::Failed { result: payload } => {
                        if payload.validate().is_err()
                            || payload.role != "tool"
                            || payload.tool_call_id.as_ref() != Some(&prior.intent.tool_call_id)
                        {
                            return Err(WorkRejection::InvalidTransition);
                        }
                        writes.push(WorkWrite::Transcript {
                            payload: payload.clone(),
                        });
                    }
                    InvocationOutcome::Cancelled {
                        evidence,
                        result: payload,
                    } => {
                        if payload.tool_call_id.as_ref() != Some(&prior.intent.tool_call_id)
                            || evidence.is_empty()
                            || payload.validate().is_err()
                            || payload.role != "tool"
                        {
                            return Err(WorkRejection::InvalidTransition);
                        }
                        writes.push(WorkWrite::Transcript {
                            payload: payload.clone(),
                        });
                    }
                }
                let mut updated = prior.clone();
                updated.status = InvocationStatus::Settled;
                updated.outcome = Some(result.outcome.clone());
                write_effect(updated, prior.revision, writes)?;
                newly_settled = next(newly_settled)?;
            }
            let mut head = facts.head.clone();
            head.unresolved_effects = head
                .unresolved_effects
                .checked_sub(newly_settled)
                .ok_or(WorkRejection::Conflict)?;
            if let Some(mut record) = processing.take() {
                if related_settled != 0 {
                    record.remaining_effects = record
                        .remaining_effects
                        .checked_sub(related_settled)
                        .ok_or(WorkRejection::Conflict)?;
                    if record.remaining_effects == 0
                        && record.stage != WorkStage::Abandoned
                        && !super::processing::is_budget_blocked(&record)
                    {
                        record.phase_sequence = next(record.phase_sequence)?;
                        record.stage = if next_work_id.is_some() {
                            WorkStage::ReasonReady
                        } else {
                            WorkStage::Settled
                        };
                        record.resume_stage = None;
                        record.checkpoint = Some(command.mutation_id.clone());
                        record.request = None;
                        record.request_id = None;
                        if record.stage == WorkStage::Settled
                            && head.current_processing_id.as_ref() == Some(&record.processing_id)
                        {
                            head.current_processing_id = None;
                        }
                    }
                    write_processing(record, receipt, writes)?;
                }
            }
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::ReconcileTaskBinding { binding, .. } => {
            if binding.recipient_lifecycle != command.recipient_lifecycle {
                return Err(WorkRejection::StaleLifecycle);
            }
            let prior = facts
                .effects
                .iter()
                .find(|effect| effect.invocation_id == binding.invocation_id)
                .ok_or(WorkRejection::Conflict)?;
            if binding.owner_task_id.is_empty()
                || binding.owner_identity != prior.intent.owner_identity
                || binding.authorization_ref != prior.intent.authorization_ref
                || binding.recovery_locator != prior.intent.recovery_locator
                || binding.initiator_session_id != command.session_id
                || binding.recipient_lifecycle != prior.recipient_lifecycle
            {
                return Err(WorkRejection::InvalidTransition);
            }
            if let Some(existing) = &prior.binding {
                return if existing == binding {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            if !matches!(
                prior.status,
                InvocationStatus::DispatchAccepted | InvocationStatus::OutcomeUnknown
            ) {
                return Err(WorkRejection::InvalidTransition);
            }
            if facts.effects.iter().any(|effect| {
                effect.invocation_id != prior.invocation_id
                    && effect.binding.as_ref().is_some_and(|bound| {
                        bound.owner_identity == binding.owner_identity
                            && bound.owner_task_id == binding.owner_task_id
                    })
            }) {
                return Err(WorkRejection::Conflict);
            }
            let mut updated = prior.clone();
            updated.binding = Some(binding.clone());
            write_effect(updated, prior.revision, writes)
        }
        WorkAction::BindTerminalObligation {
            admission_id,
            command: terminal_command,
            ..
        } => {
            let admission = facts.admission.as_ref().ok_or(WorkRejection::Conflict)?;
            if admission.admission.admission_id != *admission_id
                || admission.admission.lifecycle != command.recipient_lifecycle
                || admission.admission.session_id != command.session_id
                || terminal_command.digest().is_err()
                || !matches!(
                    terminal_command.action,
                    WorkAction::PublishDelivery { .. } | WorkAction::PublishTaskSettlement { .. }
                )
            {
                return Err(WorkRejection::InvalidTransition);
            }
            if let Some(prior) = &facts.terminal_obligation {
                return if prior.admission_id == *admission_id && prior.command == *terminal_command
                {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            writes.push(WorkWrite::TerminalObligation {
                expected_acknowledged: false,
                record: TerminalObligation {
                    admission_id: admission_id.clone(),
                    command: terminal_command.clone(),
                    acknowledgement: None,
                },
            });
            let mut head = facts.head.clone();
            head.terminal_obligations = next(head.terminal_obligations)?;
            write_head(facts, head, writes);
            Ok(())
        }
        WorkAction::AcknowledgeTerminalObligation {
            admission_id,
            receipt: acknowledgement,
            ..
        } => {
            let prior = facts
                .terminal_obligation
                .as_ref()
                .ok_or(WorkRejection::Conflict)?;
            if prior.admission_id != *admission_id
                || prior.command.session_id != acknowledgement.session_id
                || prior.command.mutation_id != acknowledgement.mutation_id
                || acknowledgement.decision != WorkDecision::Accepted
                || facts.parent_binding_receipt.as_ref() != Some(acknowledgement)
            {
                return Err(WorkRejection::Conflict);
            }
            let delivery_id = match &prior.command.action {
                WorkAction::PublishDelivery { delivery }
                | WorkAction::PublishTaskSettlement { delivery, .. } => &delivery.delivery_id,
                _ => return Err(WorkRejection::InvalidTransition),
            };
            if acknowledgement.delivery_id.as_ref() != Some(delivery_id) {
                return Err(WorkRejection::Conflict);
            }
            if let Some(existing) = &prior.acknowledgement {
                return if existing == acknowledgement {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            let mut record = prior.clone();
            record.acknowledgement = Some(acknowledgement.clone());
            writes.push(WorkWrite::TerminalObligation {
                expected_acknowledged: false,
                record,
            });
            let mut head = facts.head.clone();
            head.terminal_obligations = head
                .terminal_obligations
                .checked_sub(1)
                .ok_or(WorkRejection::Conflict)?;
            write_head(facts, head, writes);
            Ok(())
        }
        _ => Err(WorkRejection::InvalidTransition),
    }
}

pub(super) fn validate_intent(intent: &InvocationIntent) -> Result<(), WorkRejection> {
    if intent.invocation_id.is_empty()
        || intent.tool_call_id.is_empty()
        || intent.tool_name.is_empty()
        || intent.effective_tool_name.is_empty()
        || intent.owner_identity.is_empty()
        || intent.scope_id.is_empty()
        || intent.authorization_ref.is_empty()
        || intent.recovery_locator.is_empty()
        || intent.arguments.validate().is_err()
        || intent.effective_arguments.validate().is_err()
        || intent.arguments_digest != intent.arguments.sha256
        || intent.effective_arguments_digest != intent.effective_arguments.sha256
    {
        return Err(WorkRejection::InvalidTransition);
    }
    Ok(())
}

fn effect<'facts>(
    facts: &'facts WorkFacts,
    invocation_id: &str,
    revision: u64,
) -> Result<&'facts Effect, WorkRejection> {
    let effect = facts
        .effects
        .iter()
        .find(|effect| effect.invocation_id == invocation_id)
        .ok_or(WorkRejection::Conflict)?;
    if effect.revision != revision {
        return Err(WorkRejection::StaleRevision);
    }
    Ok(effect)
}

fn write_effect(
    mut effect: Effect,
    previous: u64,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    effect.revision = next(previous)?;
    writes.push(WorkWrite::Effect {
        expected_revision: Some(previous),
        record: effect,
    });
    Ok(())
}

fn processing_for_effect(
    facts: &WorkFacts,
    work_target: &WorkTarget,
    effect: &Effect,
) -> Result<Processing, WorkRejection> {
    let processing = facts.processing.as_ref().ok_or(WorkRejection::Conflict)?;
    if effect.processing_id.as_ref() != Some(&processing.processing_id)
        || processing.processing_id != work_target.work_id
        || processing.phase_sequence != effect.phase_sequence
        || work_target.expected_work_revision > processing.revision
    {
        return Err(WorkRejection::Conflict);
    }
    Ok(processing.clone())
}
