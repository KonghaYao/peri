use super::*;
use crate::session_resources::ControlStatus;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkFacts {
    pub session_id: String,
    pub control: ControlState,
    pub head: SessionWorkHead,
    pub processing: Option<Processing>,
    pub deliveries: Vec<Delivery>,
    pub effects: Vec<Effect>,
    pub drafts: Vec<StagedUserInput>,
    pub admission: Option<AdmissionRecord>,
    pub recovery_descriptor: Option<RecoveryDescriptor>,
    pub terminal_obligation: Option<TerminalObligation>,
    pub legacy_evidence: Option<LegacyEvidence>,
    pub parent_binding_receipt: Option<WorkReceipt>,
    pub parent_effect: Option<Effect>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum WorkWrite {
    Head {
        expected_change_seq: u64,
        record: SessionWorkHead,
    },
    Delivery {
        expected_revision: Option<u64>,
        record: Delivery,
    },
    Processing {
        expected_revision: Option<u64>,
        record: Processing,
    },
    Effect {
        expected_revision: Option<u64>,
        record: Effect,
    },
    Draft {
        expected_revision: Option<u64>,
        record: StagedUserInput,
    },
    Admission {
        expected_entering_mutation_id: Option<String>,
        record: AdmissionRecord,
    },
    RecoveryDescriptor {
        expected_revision: Option<u64>,
        record: RecoveryDescriptor,
    },
    TerminalObligation {
        expected_acknowledged: bool,
        record: TerminalObligation,
    },
    LegacyEvidence {
        record: LegacyEvidence,
    },
    Transcript {
        payload: WorkPayload,
    },
    Control {
        expected_revision: u64,
        record: ControlState,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkTransition {
    pub receipt: WorkReceipt,
    pub writes: Vec<WorkWrite>,
}

pub fn transition_work(
    command: &WorkCommand,
    facts: &WorkFacts,
) -> SessionResourceResult<WorkTransition> {
    command.digest()?;
    if command.session_id != facts.session_id {
        return Err(invalid("work facts belong to another session"));
    }
    if facts.deliveries.len() > MAX_WORK_PAGE_SIZE as usize
        || facts.effects.len() > MAX_WORK_PAGE_SIZE as usize
        || facts.drafts.len() > MAX_WORK_PAGE_SIZE as usize
    {
        return Err(invalid("work facts exceed command read bound"));
    }
    let mut receipt = WorkReceipt {
        session_id: command.session_id.clone(),
        mutation_id: command.mutation_id.clone(),
        before_revision: facts.head.change_seq,
        revision: facts.head.change_seq,
        decision: WorkDecision::Accepted,
        delivery_id: None,
        admission_sequence: None,
        batch_id: None,
        work_id: None,
        work_revision: None,
        stage: None,
    };
    let mut writes = Vec::new();
    let settlement = matches!(
        command.action,
        WorkAction::FinishAdmission { .. }
            | WorkAction::AcknowledgeTerminalObligation { .. }
            | WorkAction::BindTerminalObligation { .. }
            | WorkAction::ReconcileTaskBinding { .. }
            | WorkAction::CommitAct { .. }
            | WorkAction::OutcomeUnknown { .. }
    );
    let result = if command.recipient_lifecycle != facts.control.lifecycle && !settlement {
        Err(WorkRejection::StaleLifecycle)
    } else {
        apply(command, facts, &mut receipt, &mut writes)
    };
    if let Err(reason) = result {
        receipt.decision = WorkDecision::Rejected { reason };
        receipt.delivery_id = None;
        receipt.admission_sequence = None;
        receipt.batch_id = None;
        receipt.work_id = None;
        receipt.work_revision = None;
        receipt.stage = None;
        writes.clear();
    } else {
        let next = facts.head.change_seq.checked_add(1);
        match next {
            Some(change_seq) => {
                receipt.revision = change_seq;
                if let Some(WorkWrite::Head { record, .. }) = writes
                    .iter_mut()
                    .find(|write| matches!(write, WorkWrite::Head { .. }))
                {
                    record.change_seq = change_seq;
                } else {
                    let mut head = facts.head.clone();
                    head.change_seq = change_seq;
                    writes.push(WorkWrite::Head {
                        expected_change_seq: facts.head.change_seq,
                        record: head,
                    });
                }
            }
            None => {
                writes.clear();
                receipt.decision = WorkDecision::Rejected {
                    reason: WorkRejection::VersionExhausted,
                };
            }
        }
    }
    Ok(WorkTransition { receipt, writes })
}

fn apply(
    command: &WorkCommand,
    facts: &WorkFacts,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    match &command.action {
        WorkAction::StageUserInput { .. }
        | WorkAction::WithdrawStagedUserInput { .. }
        | WorkAction::PublishStagedUserInputs { .. }
        | WorkAction::PublishDelivery { .. }
        | WorkAction::PublishTaskSettlement { .. }
        | WorkAction::ClaimBatch { .. }
        | WorkAction::WithdrawDelivery { .. }
        | WorkAction::AbandonDelivery { .. } => {
            super::mailbox::apply(command, facts, receipt, writes)
        }
        WorkAction::PrepareInvocation { .. }
        | WorkAction::BeginDispatch { .. }
        | WorkAction::OutcomeUnknown { .. }
        | WorkAction::CommitAct { .. }
        | WorkAction::ReconcileTaskBinding { .. }
        | WorkAction::BindTerminalObligation { .. }
        | WorkAction::AcknowledgeTerminalObligation { .. } => {
            super::effect::apply(command, facts, receipt, writes)
        }
        _ => super::processing::apply(command, facts, receipt, writes),
    }
}

pub(super) fn guard(facts: &WorkFacts, guard: &WorkGuard) -> Result<(), WorkRejection> {
    if facts.control.control_generation != guard.expected_control_generation {
        return Err(WorkRejection::StaleControlGeneration);
    }
    if facts.control.attempt.as_ref() != Some(&guard.execution) {
        return Err(WorkRejection::StaleExecution);
    }
    if facts.control.status != ControlStatus::Active {
        return Err(WorkRejection::InvalidTransition);
    }
    if facts.head.legacy_unknown != 0 {
        return Err(WorkRejection::LegacyUnknown);
    }
    Ok(())
}

pub(super) fn target(facts: &WorkFacts, target: &WorkTarget) -> Result<Processing, WorkRejection> {
    let processing = facts
        .processing
        .as_ref()
        .ok_or(WorkRejection::InvalidTransition)?;
    if processing.recipient_lifecycle != facts.control.lifecycle {
        return Err(WorkRejection::StaleLifecycle);
    }
    if processing.processing_id != target.work_id {
        return Err(WorkRejection::Conflict);
    }
    if processing.revision != target.expected_work_revision {
        return Err(WorkRejection::StaleWorkRevision);
    }
    Ok(processing.clone())
}

pub(super) fn write_processing(
    mut processing: Processing,
    receipt: &mut WorkReceipt,
    writes: &mut Vec<WorkWrite>,
) -> Result<(), WorkRejection> {
    let previous = processing.revision;
    processing.revision = previous
        .checked_add(1)
        .ok_or(WorkRejection::VersionExhausted)?;
    receipt.work_id = Some(processing.processing_id.clone());
    receipt.work_revision = Some(processing.revision);
    receipt.stage = Some(processing.stage);
    receipt.batch_id = Some(processing.processing_id.clone());
    writes.push(WorkWrite::Processing {
        expected_revision: Some(previous),
        record: processing,
    });
    Ok(())
}

pub(super) fn next(value: u64) -> Result<u64, WorkRejection> {
    value.checked_add(1).ok_or(WorkRejection::VersionExhausted)
}

pub(super) fn write_head(facts: &WorkFacts, head: SessionWorkHead, writes: &mut Vec<WorkWrite>) {
    writes.push(WorkWrite::Head {
        expected_change_seq: facts.head.change_seq,
        record: head,
    });
}
