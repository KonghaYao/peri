use super::*;
use crate::error::WorkBudgetKind;

const BUDGET_RECOVERY_CONDITION: &str = "budget reset required";

pub(in crate::session_resources::work) fn is_budget_blocked(processing: &Processing) -> bool {
    processing.stage == WorkStage::Blocked
        && [
            WorkBudgetKind::ReasonRequests,
            WorkBudgetKind::Dispatches,
            WorkBudgetKind::Recoveries,
        ]
        .iter()
        .any(|kind| processing.blocked_evidence.as_deref() == Some(budget_reason(kind)))
}

fn budget_reason(kind: &WorkBudgetKind) -> &'static str {
    match kind {
        WorkBudgetKind::ReasonRequests => "reason budget exhausted",
        WorkBudgetKind::Dispatches => "dispatch budget exhausted",
        WorkBudgetKind::Recoveries => "recovery budget exhausted",
    }
}

pub(in crate::session_resources::work) fn block_budget(
    mut processing: Processing,
    kind: WorkBudgetKind,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    if processing.stage != WorkStage::Blocked {
        processing.resume_stage = Some(processing.stage);
    }
    processing.stage = WorkStage::Blocked;
    processing.blocked_evidence = Some(budget_reason(&kind).into());
    processing.recovery_condition = Some(BUDGET_RECOVERY_CONDITION.into());
    write_processing(processing, receipt, writes)
}

pub(in crate::session_resources::work) fn abandon_processing_inputs(
    facts: &WorkFacts,
    processing: &Processing,
    head: &mut SessionWorkHead,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    let mut abandoned = 0;
    for prior in &facts.deliveries {
        if prior.processing_id.as_ref() != Some(&processing.processing_id)
            || prior.recipient_lifecycle != processing.recipient_lifecycle
            || !matches!(
                prior.obligation,
                ObligationStatus::Pending
                    | ObligationStatus::InProgress
                    | ObligationStatus::Blocked
            )
        {
            continue;
        }
        let mut delivery = prior.clone();
        delivery.revision = next(delivery.revision)?;
        delivery.obligation = ObligationStatus::Abandoned;
        delivery.disposition = processing.blocked_evidence.clone();
        super::super::mailbox::release(head, &delivery)?;
        abandoned += u32::from(delivery.participates_in_reason);
        writes.push(WorkWrite::Delivery {
            expected_revision: Some(prior.revision),
            record: delivery,
        });
    }
    if processing.phase_sequence == 0
        && processing.response.is_none()
        && abandoned != processing.reason_delivery_count
    {
        return Err(WorkRejection::Conflict);
    }
    Ok(())
}
