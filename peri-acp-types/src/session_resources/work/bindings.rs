use super::*;

pub(super) fn validate_intent(intent: &InvocationIntent) -> Result<(), WorkRejection> {
    if intent.effective_tool_name.is_empty()
        || serde_json::from_str::<serde_json::Value>(&intent.effective_arguments_json).is_err()
        || intent.effective_arguments_digest
            != format!(
                "{:x}",
                Sha256::digest(intent.effective_arguments_json.as_bytes())
            )
    {
        return Err(WorkRejection::InvalidTransition);
    }
    if [
        &intent.invocation_id,
        &intent.tool_call_id,
        &intent.tool_name,
        &intent.owner_identity,
        &intent.scope_id,
        &intent.authorization_ref,
        &intent.recovery_locator,
    ]
    .iter()
    .any(|value| value.is_empty())
        || serde_json::from_str::<serde_json::Value>(&intent.arguments_json).is_err()
        || intent.arguments_digest
            != format!("{:x}", Sha256::digest(intent.arguments_json.as_bytes()))
    {
        return Err(WorkRejection::InvalidTransition);
    }
    Ok(())
}

pub(super) fn prepare(
    command: &WorkCommand,
    state: &mut WorkState,
    intent: &InvocationIntent,
    work_id: Option<&str>,
) -> Result<(), WorkRejection> {
    validate_intent(intent)?;
    if let Some(prior) = state.invocations.get(&intent.invocation_id) {
        if prior.intent != *intent
            || prior.recipient_lifecycle != command.recipient_lifecycle
            || (work_id.is_some() && prior.work_id.as_deref() != work_id)
        {
            return Err(WorkRejection::Conflict);
        }
        return Ok(());
    }
    state.invocations.insert(
        intent.invocation_id.clone(),
        InvocationRecord {
            intent: intent.clone(),
            recipient_lifecycle: command.recipient_lifecycle,
            work_id: work_id.map(str::to_owned),
            status: InvocationStatus::Prepared,
            outcome: None,
            unknown_reason: None,
        },
    );
    Ok(())
}

pub(super) fn reconcile(
    command: &WorkCommand,
    state: &mut WorkState,
    binding: &TaskBinding,
) -> Result<(), WorkRejection> {
    let invocation = state
        .invocations
        .get(&binding.invocation_id)
        .ok_or(WorkRejection::InvalidTransition)?;
    if binding.owner_task_id.is_empty()
        || binding.owner_identity != invocation.intent.owner_identity
        || binding.initiator_session_id != command.session_id
        || binding.recipient_lifecycle != invocation.recipient_lifecycle
        || binding.recovery_locator != invocation.intent.recovery_locator
        || binding.authorization_ref != invocation.intent.authorization_ref
    {
        return Err(WorkRejection::Conflict);
    }
    if let Some(prior) = state.task_bindings.get(&binding.invocation_id) {
        if prior != binding {
            return Err(WorkRejection::Conflict);
        }
    }
    if state.task_bindings.values().any(|prior| {
        prior.owner_identity == binding.owner_identity
            && prior.owner_task_id == binding.owner_task_id
            && prior.invocation_id != binding.invocation_id
    }) {
        return Err(WorkRejection::Conflict);
    }
    state
        .task_bindings
        .insert(binding.invocation_id.clone(), binding.clone());
    Ok(())
}
