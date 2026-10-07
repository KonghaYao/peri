use super::*;
use crate::session_resources::ControlStatus;

pub struct WorkReduction {
    /// The post-command state, `Some` exactly when `receipt.decision` is accepted.
    ///
    /// `apply` mutates the state in place and can fail after partially mutating it
    /// (for example `PublishStagedUserInputs` publishes deliveries one by one and
    /// can still report a conflict), so a rejected command hands back no state at
    /// all: the partially mutated state is dropped here and the durable state
    /// stays whatever the database already holds. Use [`WorkReduction::accepted_state`]
    /// to assert that pairing before persisting anything.
    pub state: Option<WorkState>,
    pub receipt: WorkReceipt,
    pub projections: Vec<WorkPayload>,
    pub events: Vec<WorkEvent>,
    pub control: Option<ControlState>,
}

impl WorkReduction {
    /// The state to persist, `None` when the command was rejected.
    ///
    /// Returns an error when `state` and the receipt disagree with each other,
    /// so a rejected reduction can never be persisted as if it were accepted.
    pub fn accepted_state(&self) -> SessionResourceResult<Option<&WorkState>> {
        match (&self.receipt.decision, self.state.as_ref()) {
            (WorkDecision::Accepted, Some(state)) => Ok(Some(state)),
            (WorkDecision::Rejected { .. }, None) => Ok(None),
            (WorkDecision::Accepted, None) => {
                Err(invalid("accepted work reduction carries no state"))
            }
            (WorkDecision::Rejected { .. }, Some(_)) => {
                Err(invalid("rejected work reduction carries state"))
            }
        }
    }
}

/// Reduce one command into an owned snapshot.
///
/// Takes ownership of `current` and mutates it in place: callers already hold an
/// exclusive snapshot (`read_snapshot` / `read_work_snapshot` return an owned
/// `WorkState`), so no copy of the state — which embeds historical
/// `works[].reasonRequest.serializedRequest` payloads — is made here.
pub fn reduce_work(
    command: &WorkCommand,
    control: &ControlState,
    current: WorkState,
) -> SessionResourceResult<WorkReduction> {
    command.digest()?;
    if let Ok(Some(receipt)) = super::admission::prior_receipt(command, &current) {
        let accepted = receipt.decision == WorkDecision::Accepted;
        return Ok(WorkReduction {
            state: accepted.then_some(current),
            receipt,
            projections: Vec::new(),
            events: Vec::new(),
            control: None,
        });
    }
    let mut receipt = WorkReceipt {
        session_id: command.session_id.clone(),
        mutation_id: command.mutation_id.clone(),
        before_revision: current.revision,
        revision: current.revision,
        decision: WorkDecision::Accepted,
        delivery_id: None,
        admission_sequence: None,
        batch_id: None,
        work_id: None,
        work_revision: None,
        stage: None,
    };
    let mut state = current;
    let mut projections = Vec::new();
    let mut events = Vec::new();
    let mut next_control = None;
    let result = super::admission::prior_receipt(command, &state).and_then(|_| {
        apply(
            command,
            control,
            &mut state,
            &mut receipt,
            &mut projections,
            &mut events,
            &mut next_control,
        )
    });
    let result = result.and_then(|()| {
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or(WorkRejection::VersionExhausted)?;
        Ok(())
    });
    match result {
        Ok(()) => {
            receipt.revision = state.revision;
            match &command.action {
                WorkAction::RegisterAdmission { admission } => {
                    state
                        .admissions
                        .get_mut(&admission.admission_id)
                        .ok_or_else(|| invalid("missing admission record"))?
                        .entering_receipt = Some(receipt.clone())
                }
                WorkAction::FinishAdmission { admission, .. } => {
                    state
                        .admissions
                        .get_mut(&admission.admission_id)
                        .ok_or_else(|| invalid("missing admission record"))?
                        .settled_receipt = Some(receipt.clone())
                }
                WorkAction::PublishStagedUserInputs { .. } => {
                    state
                        .user_input_publications
                        .insert(command.mutation_id.clone(), command.clone());
                }
                _ => {}
            }
            Ok(WorkReduction {
                state: Some(state),
                receipt,
                projections,
                events,
                control: next_control,
            })
        }
        Err(reason) => {
            // `state` may be partially applied; discard it instead of handing a
            // half-applied state back to the caller.
            drop(state);
            projections.clear();
            events.clear();
            next_control = None;
            receipt.decision = WorkDecision::Rejected { reason };
            receipt.delivery_id = None;
            receipt.admission_sequence = None;
            receipt.batch_id = None;
            receipt.work_id = None;
            receipt.work_revision = None;
            receipt.stage = None;
            Ok(WorkReduction {
                state: None,
                receipt,
                projections,
                events,
                control: next_control,
            })
        }
    }
}

fn apply(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    receipt: &mut WorkReceipt,
    projections: &mut Vec<WorkPayload>,
    events: &mut Vec<WorkEvent>,
    next_control: &mut Option<ControlState>,
) -> Result<(), WorkRejection> {
    match &command.action {
        WorkAction::BindTerminalObligation {
            expected_revision,
            admission_id,
            command: parent_command,
        } => {
            revision_guard(state, *expected_revision)?;
            return super::admission::bind_terminal(
                command,
                control,
                state,
                admission_id,
                parent_command,
            );
        }
        WorkAction::AcknowledgeTerminalObligation {
            expected_revision,
            admission_id,
            receipt: parent_receipt,
        } => {
            revision_guard(state, *expected_revision)?;
            return super::admission::acknowledge_terminal(
                command,
                state,
                admission_id,
                parent_receipt,
            );
        }
        _ => {}
    }
    if let WorkAction::FinishAdmission {
        admission,
        evidence_id,
    } = &command.action
    {
        *next_control =
            super::admission::finish(command, control, state, admission, evidence_id, receipt)?;
        return Ok(());
    }
    if let WorkAction::PublishDelivery { delivery } = &command.action {
        return super::delivery::publish(command, control, state, delivery, receipt, events);
    }
    if let WorkAction::PublishTaskSettlement { delivery, binding } = &command.action {
        if binding.initiator_session_id != command.session_id
            || binding.recipient_lifecycle != command.recipient_lifecycle
            || state.task_bindings.get(&binding.invocation_id) != Some(binding)
            || !matches!(
                delivery.purpose,
                DeliveryPurpose::TaskTerminal | DeliveryPurpose::Settlement
            )
        {
            return Err(WorkRejection::Conflict);
        }
        return super::delivery::publish(command, control, state, delivery, receipt, events);
    }
    if let WorkAction::ReconcileTaskBinding {
        expected_revision,
        binding,
    } = &command.action
    {
        return super::bindings::reconcile(command, state, *expected_revision, binding);
    }
    if command.recipient_lifecycle != control.lifecycle {
        return Err(WorkRejection::StaleLifecycle);
    }
    let target = match &command.action {
        WorkAction::BeginReason { target, .. }
        | WorkAction::CommitReasonResponseAndDispatchIntent { target, .. }
        | WorkAction::BeginDispatch { target, .. }
        | WorkAction::CommitAct { target, .. }
        | WorkAction::ResumeWork { target, .. }
        | WorkAction::OutcomeUnknown { target, .. }
        | WorkAction::BlockWork { target, .. }
        | WorkAction::AbandonWork { target, .. }
        | WorkAction::SettleWork { target, .. } => Some(target),
        _ => None,
    };
    if let Some(target) = target {
        if state.works.contains_key(&target.work_id) {
            match state.work_lifecycle(&target.work_id) {
                Some(lifecycle) if lifecycle != command.recipient_lifecycle => {
                    return Err(WorkRejection::StaleLifecycle)
                }
                None => return Err(WorkRejection::LegacyUnknown),
                _ => {}
            }
        }
    }
    match &command.action {
        WorkAction::StageUserInput {
            input_json,
            command_id,
            fingerprint,
        } => super::user_input::stage(
            command,
            control,
            state,
            input_json,
            command_id,
            *fingerprint,
        ),
        WorkAction::WithdrawStagedUserInput {
            input_id,
            command_id,
            fingerprint,
        } => super::user_input::withdraw(command, state, input_id, command_id, *fingerprint),
        WorkAction::PublishStagedUserInputs {
            expected_revision,
            expected_control_generation,
            expected_attempt,
            interrupt_current,
            deliveries,
        } => {
            revision_guard(state, *expected_revision)?;
            if *expected_control_generation != control.control_generation {
                return Err(WorkRejection::StaleControlGeneration);
            }
            if *expected_attempt != control.attempt {
                return Err(WorkRejection::StaleExecution);
            }
            super::user_input::publish(
                command,
                control,
                state,
                deliveries,
                *interrupt_current,
                receipt,
                events,
            )?;
            if *interrupt_current && control.status == ControlStatus::Paused {
                let resumed = crate::session_resources::control::decide_control(
                    &crate::session_resources::ControlCommand {
                        session_id: command.session_id.clone(),
                        command_id: format!("{}:resume", command.mutation_id),
                        expected_lifecycle: control.lifecycle,
                        expected_revision: control.revision,
                        expected_control_generation: control.control_generation,
                        action: crate::session_resources::ControlAction::Resume,
                    },
                    control,
                );
                if resumed.decision != crate::session_resources::ControlDecision::Accepted {
                    return Err(WorkRejection::InvalidTransition);
                }
                *next_control = Some(resumed.state);
            }
            Ok(())
        }
        WorkAction::RegisterAdmission { admission } => {
            *next_control = Some(super::admission::register(
                command, control, state, admission, receipt,
            )?);
            Ok(())
        }
        WorkAction::BindResourceOwners {
            expected_revision,
            connections_json,
            authorization_ref,
        } => {
            revision_guard(state, *expected_revision)?;
            super::admission::bind_resources(command, state, connections_json, authorization_ref)
        }
        WorkAction::BindChildResumeMetadata {
            expected_revision,
            metadata_json,
        } => {
            revision_guard(state, *expected_revision)?;
            super::admission::bind_child_metadata(command, state, metadata_json)
        }
        WorkAction::BindWorkDelegation {
            expected_revision,
            work_id,
            binding,
            parent_binding_receipt,
        } => {
            revision_guard(state, *expected_revision)?;
            super::admission::bind_work_delegation(
                command,
                control,
                state,
                work_id,
                binding,
                parent_binding_receipt,
            )
        }
        WorkAction::ClaimBatch {
            guard,
            batch_id,
            delivery_ids,
        } => {
            execution_guard(command, control, state, guard, true)?;
            super::delivery::claim(
                command,
                state,
                guard,
                batch_id,
                delivery_ids,
                receipt,
                projections,
            )
        }
        WorkAction::BeginReason {
            guard,
            target,
            request_id,
            request,
        } => {
            execution_guard(command, control, state, guard, true)?;
            super::processing::begin_reason(state, target, request_id, request, receipt)
        }
        WorkAction::CommitReasonResponseAndDispatchIntent { guard, .. } => {
            execution_guard(command, control, state, guard, false)?;
            super::processing::commit_reason(command, state, receipt, projections)
        }
        WorkAction::BeginDispatch {
            guard,
            target,
            invocation_id,
        } => {
            execution_guard(command, control, state, guard, true)?;
            super::processing::begin_dispatch(state, target, invocation_id, receipt)
        }
        WorkAction::CommitAct {
            guard,
            target,
            results,
            next_work_id,
        } => {
            if next_work_id.is_some() {
                execution_guard(command, control, state, guard, false)?;
            } else {
                settlement_guard(command, control, state, guard, target)?;
            }
            super::processing::commit_act(
                command,
                state,
                target,
                results,
                next_work_id.as_deref(),
                receipt,
                projections,
            )
        }
        WorkAction::ResumeWork {
            guard,
            target,
            recovery_evidence,
        } => {
            execution_guard(command, control, state, guard, true)?;
            super::processing::resume(state, target, recovery_evidence, receipt)
        }
        WorkAction::OutcomeUnknown {
            expected_revision,
            target,
            invocation_id,
            reason,
        } => {
            revision_guard(state, *expected_revision)?;
            super::processing::unknown(state, target, invocation_id, reason, receipt)
        }
        WorkAction::BlockWork {
            expected_revision,
            target,
            reason,
            recovery_condition,
        } => {
            revision_guard(state, *expected_revision)?;
            super::processing::block(state, target, reason, recovery_condition, receipt)
        }
        WorkAction::AbandonWork {
            expected_revision,
            expected_control_generation,
            target,
            reason,
            authorization_ref,
        } => {
            revision_guard(state, *expected_revision)?;
            if control.control_generation != *expected_control_generation {
                return Err(WorkRejection::StaleControlGeneration);
            }
            super::processing::abandon(state, target, reason, authorization_ref, receipt)
        }
        WorkAction::SettleWork {
            expected_revision,
            target,
        } => {
            revision_guard(state, *expected_revision)?;
            super::processing::settle(state, target, receipt)
        }
        WorkAction::PrepareInvocation {
            expected_revision,
            intent,
        } => {
            revision_guard(state, *expected_revision)?;
            if control.status != ControlStatus::Active {
                return Err(WorkRejection::InvalidTransition);
            }
            super::bindings::prepare(command, state, intent, None)
        }
        WorkAction::ReconcileTaskBinding {
            expected_revision,
            binding,
        } => super::bindings::reconcile(command, state, *expected_revision, binding),
        WorkAction::WithdrawDelivery {
            expected_revision,
            expected_control_generation,
            delivery_id,
            authorization_ref,
        } => {
            revision_guard(state, *expected_revision)?;
            if expected_control_generation
                .is_some_and(|generation| generation != control.control_generation)
            {
                return Err(WorkRejection::StaleControlGeneration);
            }
            super::delivery::withdraw(state, delivery_id, authorization_ref, receipt)?;
            super::user_input::withdraw_published(
                state,
                delivery_id,
                expected_control_generation.is_some(),
            )?;
            Ok(())
        }
        WorkAction::AbandonDelivery {
            guard,
            delivery_id,
            reason,
            evidence,
        } => {
            execution_guard(command, control, state, guard, true)?;
            super::delivery::abandon_delivery(
                command,
                state,
                delivery_id,
                reason,
                evidence,
                receipt,
            )
        }
        WorkAction::ResetBudget {
            expected_revision,
            budget_id,
            authorization_ref,
        } => {
            revision_guard(state, *expected_revision)?;
            if authorization_ref.is_empty() || !state.budgets.contains_key(budget_id) {
                return Err(WorkRejection::InvalidTransition);
            }
            state
                .budgets
                .insert(budget_id.clone(), WorkBudget::default());
            Ok(())
        }
        WorkAction::QuarantineLegacy {
            expected_revision,
            record_id,
            evidence,
        } => {
            revision_guard(state, *expected_revision)?;
            if record_id.is_empty() || evidence.is_empty() {
                return Err(WorkRejection::InvalidTransition);
            }
            if state
                .legacy_unknown
                .get(record_id)
                .is_some_and(|existing| existing != evidence)
            {
                return Err(WorkRejection::Conflict);
            }
            state
                .legacy_unknown
                .insert(record_id.clone(), evidence.clone());
            Ok(())
        }
        WorkAction::PublishDelivery { .. }
        | WorkAction::PublishTaskSettlement { .. }
        | WorkAction::BindTerminalObligation { .. }
        | WorkAction::AcknowledgeTerminalObligation { .. }
        | WorkAction::FinishAdmission { .. } => unreachable!(),
    }
}

pub(super) fn revision_guard(state: &WorkState, revision: u64) -> Result<(), WorkRejection> {
    if state.revision != revision {
        return Err(WorkRejection::StaleRevision);
    }
    Ok(())
}

fn settlement_guard(
    command: &WorkCommand,
    control: &ControlState,
    state: &WorkState,
    guard: &WorkGuard,
    target: &WorkTarget,
) -> Result<(), WorkRejection> {
    revision_guard(state, guard.expected_revision)?;
    if command.recipient_lifecycle != control.lifecycle {
        return Err(WorkRejection::StaleLifecycle);
    }
    let work = state
        .works
        .get(&target.work_id)
        .ok_or(WorkRejection::Conflict)?;
    let batch = state
        .batches
        .get(&work.batch_id)
        .ok_or(WorkRejection::Conflict)?;
    if batch.recipient_lifecycle != command.recipient_lifecycle {
        return Err(WorkRejection::StaleLifecycle);
    }
    let admitted = state.admissions.values().any(|record| {
        record.admission.session_id == command.session_id
            && record.admission.lifecycle == command.recipient_lifecycle
            && record.admission.execution == guard.execution
            && record
                .entering_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.decision == WorkDecision::Accepted)
            && state
                .admission_batch(&record.admission)
                .is_some_and(|admitted_batch| admitted_batch.batch_id == work.batch_id)
    });
    if batch.execution != guard.execution && !admitted {
        return Err(WorkRejection::StaleExecution);
    }
    Ok(())
}

fn execution_guard(
    command: &WorkCommand,
    control: &ControlState,
    state: &WorkState,
    guard: &WorkGuard,
    require_active: bool,
) -> Result<(), WorkRejection> {
    revision_guard(state, guard.expected_revision)?;
    if control.control_generation != guard.expected_control_generation {
        return Err(WorkRejection::StaleControlGeneration);
    }
    if control.attempt.as_ref() != Some(&guard.execution) {
        return Err(WorkRejection::StaleExecution);
    }
    if require_active && control.status != ControlStatus::Active {
        return Err(WorkRejection::InvalidTransition);
    }
    if !state.legacy_unknown.is_empty() {
        return Err(WorkRejection::LegacyUnknown);
    }
    if command.recipient_lifecycle != control.lifecycle {
        return Err(WorkRejection::StaleLifecycle);
    }
    Ok(())
}

pub(super) fn work_mut<'state>(
    state: &'state mut WorkState,
    target: &WorkTarget,
) -> Result<&'state mut WorkRecord, WorkRejection> {
    let work = state
        .works
        .get_mut(&target.work_id)
        .ok_or(WorkRejection::InvalidTransition)?;
    if work.revision != target.expected_work_revision {
        return Err(WorkRejection::StaleWorkRevision);
    }
    Ok(work)
}

pub(super) fn bump(work: &mut WorkRecord, receipt: &mut WorkReceipt) -> Result<(), WorkRejection> {
    work.revision = work
        .revision
        .checked_add(1)
        .ok_or(WorkRejection::VersionExhausted)?;
    receipt.work_id = Some(work.work_id.clone());
    receipt.work_revision = Some(work.revision);
    receipt.batch_id = Some(work.batch_id.clone());
    receipt.stage = Some(work.stage);
    Ok(())
}
