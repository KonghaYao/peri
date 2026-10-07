use super::*;

pub(super) fn bind(
    command: &WorkCommand,
    facts: &WorkFacts,
    work_id: &str,
    binding: &TaskBinding,
    parent_binding_receipt: &WorkReceipt,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    if binding.recipient_lifecycle == 0
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
    if let Some(prior) = &facts.processing {
        if prior.processing_id != work_id
            || prior.recipient_lifecycle != command.recipient_lifecycle
            || prior
                .delegation
                .as_ref()
                .is_some_and(|bound| bound != &delegation)
        {
            return Err(WorkRejection::Conflict);
        }
        let mut processing = prior.clone();
        processing.delegation = Some(delegation);
        return write_processing(processing, receipt, writes);
    }
    if facts.control.status != ControlStatus::Active || facts.head.legacy_unknown != 0 {
        return Err(WorkRejection::Conflict);
    }
    let delivery_ids = super::candidate::pending_delivery_ids(command, facts, work_id)?;
    for delivery_id in &delivery_ids {
        let prior = facts
            .deliveries
            .iter()
            .find(|delivery| &delivery.delivery_id == delivery_id)
            .ok_or(WorkRejection::Conflict)?;
        if prior
            .delegation
            .as_ref()
            .is_some_and(|bound| bound != &delegation)
        {
            return Err(WorkRejection::Conflict);
        }
        let mut delivery = prior.clone();
        delivery.revision = next(delivery.revision)?;
        delivery.delegation = Some(delegation.clone());
        writes.push(WorkWrite::Delivery {
            expected_revision: Some(prior.revision),
            record: delivery,
        });
    }
    receipt.work_id = Some(work_id.into());
    receipt.work_revision = Some(0);
    receipt.batch_id = Some(work_id.into());
    Ok(())
}
