use peri_acp_types::session_resources::work::{
    InvocationIntent, TaskBinding, WorkInspection, WorkPage,
};

pub(crate) struct RecoveredTaskBinding {
    pub binding: TaskBinding,
    pub intent: InvocationIntent,
}

pub(crate) fn recover_task_binding(
    snapshot: &WorkInspection,
    owner_identity: &str,
    owner_task_id: &str,
) -> Result<RecoveredTaskBinding, String> {
    let WorkPage::Effects(effects) = &snapshot.page else {
        return Err("Unroutable: expected exact immutable effect query".into());
    };
    let mut found = effects
        .iter()
        .filter_map(|effect| effect.binding.as_ref())
        .filter(|binding| {
            binding.owner_identity == owner_identity && binding.owner_task_id == owner_task_id
        });
    let binding = found
        .next()
        .ok_or("OutcomeUnknown: owner task has no durable immutable binding")?;
    if found.next().is_some()
        || binding.initiator_session_id != snapshot.session_id
        || binding.recipient_lifecycle == 0
    {
        return Err(
            "Unroutable: task binding identity is ambiguous or conflicts with initiator".into(),
        );
    }
    let record = effects
        .iter()
        .find(|effect| effect.invocation_id == binding.invocation_id)
        .ok_or("Unroutable: task binding has no immutable invocation intent")?;
    if record.recipient_lifecycle != binding.recipient_lifecycle
        || record.intent.owner_identity != binding.owner_identity
        || record.intent.recovery_locator != binding.recovery_locator
        || record.intent.authorization_ref != binding.authorization_ref
        || record.intent.invocation_id != binding.invocation_id
        || record.intent.scope_id != binding.initiator_session_id
    {
        return Err(
            "Unroutable: task binding conflicts with its immutable invocation intent".into(),
        );
    }
    Ok(RecoveredTaskBinding {
        binding: binding.clone(),
        intent: record.intent.clone(),
    })
}

#[cfg(test)]
#[path = "invocation_recovery_test.rs"]
mod tests;
