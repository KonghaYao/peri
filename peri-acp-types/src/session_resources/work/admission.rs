use super::*;
use crate::session_resources::ControlStatus;

pub(super) fn prior_receipt(
    command: &WorkCommand,
    state: &WorkState,
) -> Result<Option<WorkReceipt>, WorkRejection> {
    let (admission, finishing, evidence) = match &command.action {
        WorkAction::RegisterAdmission { admission } => (admission, false, None),
        WorkAction::FinishAdmission {
            admission,
            evidence_id,
        } => (admission, true, Some(evidence_id)),
        _ => return Ok(None),
    };
    if let Some(prior) = state.admissions.get(&admission.admission_id) {
        if prior.admission != *admission {
            return Err(WorkRejection::Conflict);
        }
        if finishing {
            if prior
                .evidence_id
                .as_ref()
                .is_some_and(|saved| Some(saved) != evidence)
            {
                return Err(WorkRejection::Conflict);
            }
            return Ok(prior.settled_receipt.clone());
        }
        return Ok(prior.entering_receipt.clone());
    }
    Ok(None)
}

pub(super) fn register(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    admission: &WorkAdmission,
    receipt: &mut WorkReceipt,
) -> Result<ControlState, WorkRejection> {
    if admission.session_id != command.session_id
        || admission.lifecycle != command.recipient_lifecycle
        || admission.lifecycle != control.lifecycle
    {
        return Err(WorkRejection::StaleLifecycle);
    }
    if admission.control_generation != control.control_generation {
        return Err(WorkRejection::StaleControlGeneration);
    }
    if control.status != ControlStatus::Active {
        return Err(WorkRejection::InvalidTransition);
    }
    let snapshot = WorkSnapshot::from_state(
        &WorkQuery {
            session_id: command.session_id.clone(),
            limit: 1,
        },
        control.clone(),
        state.clone(),
    );
    if snapshot.validate_admission(admission).is_err() {
        return Err(WorkRejection::Conflict);
    }
    if control
        .attempt
        .as_ref()
        .is_some_and(|existing| existing != &admission.execution)
    {
        return Err(WorkRejection::StaleExecution);
    }
    let mut next = control.clone();
    if next.attempt.is_none() {
        next.attempt = Some(admission.execution.clone());
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or(WorkRejection::VersionExhausted)?;
    }
    state.admissions.insert(
        admission.admission_id.clone(),
        AdmissionRecord {
            admission: admission.clone(),
            entering_receipt: None,
            settled_receipt: None,
            evidence_id: None,
        },
    );
    receipt.work_id = Some(admission.work_id.clone());
    receipt.work_revision = Some(admission.work_revision);
    Ok(next)
}

pub(super) fn finish(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    admission: &WorkAdmission,
    evidence_id: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    if admission.session_id != command.session_id
        || admission.lifecycle != command.recipient_lifecycle
        || evidence_id.is_empty()
        || control.attempt.is_some()
        || (state
            .terminal_obligations
            .contains_key(&admission.admission_id)
            && !state
                .terminal_acknowledgements
                .contains_key(&admission.admission_id))
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let record = state
        .admissions
        .get_mut(&admission.admission_id)
        .ok_or(WorkRejection::InvalidTransition)?;
    if record.admission != *admission || record.entering_receipt.is_none() {
        return Err(WorkRejection::Conflict);
    }
    record.evidence_id = Some(evidence_id.into());
    receipt.work_id = Some(admission.work_id.clone());
    receipt.work_revision = Some(admission.work_revision);
    Ok(())
}

pub(super) fn bind_terminal(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    admission_id: &str,
    parent_command: &WorkCommand,
) -> Result<(), WorkRejection> {
    let admission = state
        .admissions
        .get(admission_id)
        .ok_or(WorkRejection::Conflict)?;
    if admission.admission.session_id != command.session_id
        || admission.admission.lifecycle != command.recipient_lifecycle
        || admission.entering_receipt.is_none()
        || admission.settled_receipt.is_some()
        || control.attempt.is_some()
        || parent_command.session_id == command.session_id
        || parent_command.digest().is_err()
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let WorkAction::PublishTaskSettlement { delivery, binding } = &parent_command.action else {
        return Err(WorkRejection::InvalidTransition);
    };
    let delegation = state
        .work_delegations
        .get(&admission.admission.work_id)
        .ok_or(WorkRejection::LegacyUnknown)?;
    if delegation != binding {
        return Err(WorkRejection::Conflict);
    }
    if binding.initiator_session_id != parent_command.session_id
        || binding.recipient_lifecycle != parent_command.recipient_lifecycle
        || !matches!(
            delivery.purpose,
            DeliveryPurpose::TaskTerminal | DeliveryPurpose::Settlement
        )
    {
        return Err(WorkRejection::Conflict);
    }
    if state
        .terminal_obligations
        .get(admission_id)
        .is_some_and(|prior| prior != parent_command)
    {
        return Err(WorkRejection::Conflict);
    }
    state
        .terminal_obligations
        .insert(admission_id.into(), parent_command.clone());
    Ok(())
}

pub(super) fn acknowledge_terminal(
    command: &WorkCommand,
    state: &mut WorkState,
    admission_id: &str,
    receipt: &WorkReceipt,
) -> Result<(), WorkRejection> {
    let admission = state
        .admissions
        .get(admission_id)
        .ok_or(WorkRejection::Conflict)?;
    let parent_command = state
        .terminal_obligations
        .get(admission_id)
        .ok_or(WorkRejection::Conflict)?;
    if admission.admission.session_id != command.session_id
        || admission.admission.lifecycle != command.recipient_lifecycle
        || receipt.decision != WorkDecision::Accepted
        || receipt.session_id != parent_command.session_id
        || receipt.mutation_id != parent_command.mutation_id
    {
        return Err(WorkRejection::Conflict);
    }
    if state
        .terminal_acknowledgements
        .get(admission_id)
        .is_some_and(|prior| prior != receipt)
    {
        return Err(WorkRejection::Conflict);
    }
    state
        .terminal_acknowledgements
        .insert(admission_id.into(), receipt.clone());
    Ok(())
}

pub(super) fn bind_resources(
    command: &WorkCommand,
    state: &mut WorkState,
    connections_json: &str,
    authorization_ref: &str,
) -> Result<(), WorkRejection> {
    let value: serde_json::Value =
        serde_json::from_str(connections_json).map_err(|_| WorkRejection::InvalidTransition)?;
    if authorization_ref.is_empty() || (!value.is_object() && !value.is_array()) {
        return Err(WorkRejection::InvalidTransition);
    }
    let connections_json =
        serde_json::to_string(&value).map_err(|_| WorkRejection::InvalidTransition)?;
    let binding = ResourceOwnerBinding {
        recipient_lifecycle: command.recipient_lifecycle,
        connections_json,
        authorization_ref: authorization_ref.into(),
    };
    if state
        .resource_owners
        .get(&command.recipient_lifecycle)
        .is_some_and(|prior| prior != &binding)
    {
        return Err(WorkRejection::Conflict);
    }
    state
        .resource_owners
        .insert(command.recipient_lifecycle, binding);
    Ok(())
}

pub(super) fn bind_child_metadata(
    command: &WorkCommand,
    state: &mut WorkState,
    metadata_json: &str,
) -> Result<(), WorkRejection> {
    let value: serde_json::Value =
        serde_json::from_str(metadata_json).map_err(|_| WorkRejection::InvalidTransition)?;
    if !value.is_object() || value.as_object().is_none_or(|object| object.is_empty()) {
        return Err(WorkRejection::InvalidTransition);
    }
    let canonical = serde_json::to_string(&value).map_err(|_| WorkRejection::InvalidTransition)?;
    if state
        .child_resume_metadata
        .get(&command.recipient_lifecycle)
        .is_some_and(|prior| prior != &canonical)
    {
        return Err(WorkRejection::Conflict);
    }
    state
        .child_resume_metadata
        .insert(command.recipient_lifecycle, canonical);
    Ok(())
}

pub(super) fn bind_work_delegation(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    work_id: &str,
    binding: &TaskBinding,
    parent_receipt: &WorkReceipt,
) -> Result<(), WorkRejection> {
    if state.works.contains_key(work_id)
        && state.work_lifecycle(work_id) != Some(command.recipient_lifecycle)
    {
        return Err(WorkRejection::StaleLifecycle);
    }
    let snapshot = WorkSnapshot::from_state(
        &WorkQuery {
            session_id: command.session_id.clone(),
            limit: 64,
        },
        control.clone(),
        state.clone(),
    );
    if work_id.is_empty()
        || (!state.works.contains_key(work_id)
            && !snapshot
                .candidates
                .iter()
                .any(|candidate| candidate.work_id == work_id))
        || binding.initiator_session_id == command.session_id
        || parent_receipt.decision != WorkDecision::Accepted
        || parent_receipt.session_id != binding.initiator_session_id
        || parent_receipt.mutation_id.is_empty()
    {
        return Err(WorkRejection::Conflict);
    }
    if state
        .work_delegations
        .get(work_id)
        .is_some_and(|prior| prior != binding)
    {
        return Err(WorkRejection::Conflict);
    }
    state
        .work_delegations
        .insert(work_id.into(), binding.clone());
    Ok(())
}
