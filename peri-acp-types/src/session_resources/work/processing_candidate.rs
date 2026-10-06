use super::*;

pub(super) fn pending_delivery_ids(
    command: &WorkCommand,
    facts: &WorkFacts,
    work_id: &str,
) -> Result<Vec<String>, WorkRejection> {
    if work_id
        != pending_processing_id(
            &command.session_id,
            command.recipient_lifecycle,
            facts.head.next_delivery_seq,
        )
        || facts.head.current_processing_id.is_some()
        || facts.deliveries.is_empty()
        || facts.deliveries.len() as u64 > facts.head.limits.max_batch_size
    {
        return Err(WorkRejection::Conflict);
    }
    let mut deliveries: Vec<_> = facts.deliveries.iter().collect();
    deliveries.sort_by(|first, second| {
        (first.admission_sequence, &first.delivery_id)
            .cmp(&(second.admission_sequence, &second.delivery_id))
    });
    let mut delivery_ids = Vec::new();
    for delivery in deliveries {
        if delivery.recipient_lifecycle != command.recipient_lifecycle
            || delivery.obligation != ObligationStatus::Pending
            || delivery.processing_id.is_some()
            || delivery.disposition.is_some()
            || delivery_ids.contains(&delivery.delivery_id)
        {
            return Err(WorkRejection::Conflict);
        }
        delivery_ids.push(delivery.delivery_id.clone());
    }
    Ok(delivery_ids)
}
