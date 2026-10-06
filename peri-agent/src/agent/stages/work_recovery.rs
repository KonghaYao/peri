use peri_acp_types::session_resources::work::{
    InvocationIntent, InvocationOutcome, InvocationResult, InvocationStatus, ReasonRequest,
    WorkPayload, WorkSnapshot, WorkStage, WorkTarget,
};
use peri_acp_types::store::{deserialize_persisted_payload, PersistedPayload};

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkRecoveryError {
    #[error("legacy records require explicit recovery evidence")]
    LegacyUnknown { record_ids: Vec<String> },
    #[error("durable work record is missing")]
    MissingWork,
    #[error("durable work batch does not match its delivery records")]
    InvalidBatch,
    #[error("durable work does not have a confirmed current recipient lifecycle")]
    UnconfirmedLifecycle,
    #[error("durable work projection is incomplete or inconsistent")]
    InvalidProjection,
    #[error("durable model response or request checkpoint is missing")]
    MissingCheckpoint,
    #[error("durable invocation record is incomplete or inconsistent")]
    InvalidInvocation,
}

#[derive(Debug)]
pub(crate) enum RecoveredStage {
    ReasonReady,
    ReasonUncertain {
        request_id: String,
        request: ReasonRequest,
    },
    ActReady {
        response: PersistedPayload,
        prepared: Vec<InvocationIntent>,
        settled: Vec<InvocationResult>,
    },
    ReconcileInvocations {
        invocation_ids: Vec<String>,
    },
    Blocked {
        reason: Option<String>,
        recovery_condition: Option<String>,
    },
    Finished,
}

#[derive(Debug)]
pub(crate) struct RecoveredWork {
    pub(crate) target: WorkTarget,
    pub(crate) budget_id: String,
    pub(crate) batch_id: String,
    pub(crate) delivery_ids: Vec<String>,
    pub(crate) processing_delivery_ids: Vec<String>,
    pub(crate) projection: Vec<PersistedPayload>,
    pub(crate) stage: RecoveredStage,
}

fn decode_payload(payload: &WorkPayload) -> Result<PersistedPayload, WorkRecoveryError> {
    payload
        .validate()
        .map_err(|_| WorkRecoveryError::InvalidProjection)?;
    deserialize_persisted_payload(&payload.serialized)
        .map_err(|_| WorkRecoveryError::InvalidProjection)
}

pub(crate) fn recover_work(
    snapshot: &WorkSnapshot,
    work_id: &str,
) -> Result<RecoveredWork, WorkRecoveryError> {
    if !snapshot.state.legacy_unknown.is_empty() {
        return Err(WorkRecoveryError::LegacyUnknown {
            record_ids: snapshot.state.legacy_unknown.keys().cloned().collect(),
        });
    }
    let work = snapshot
        .state
        .works
        .get(work_id)
        .ok_or(WorkRecoveryError::MissingWork)?;
    if work.work_id != work_id || !snapshot.state.budgets.contains_key(&work.budget_id) {
        return Err(WorkRecoveryError::MissingWork);
    }
    snapshot
        .validate_work_lifecycle(work_id)
        .map_err(|_| WorkRecoveryError::UnconfirmedLifecycle)?;
    let batch = snapshot
        .state
        .batches
        .get(&work.batch_id)
        .ok_or(WorkRecoveryError::InvalidBatch)?;
    if batch.batch_id != work.batch_id {
        return Err(WorkRecoveryError::InvalidBatch);
    }
    let mut projection = Vec::new();
    for delivery_id in &batch.delivery_ids {
        let delivery = snapshot
            .state
            .deliveries
            .get(delivery_id)
            .ok_or(WorkRecoveryError::InvalidBatch)?;
        if delivery.batch_id.as_ref() != Some(&batch.batch_id)
            || delivery.recipient_lifecycle != batch.recipient_lifecycle
        {
            return Err(WorkRecoveryError::InvalidBatch);
        }
        if let Some(version) = batch.projection_versions.get(delivery_id) {
            if !delivery.projected || delivery.projection_version != *version {
                return Err(WorkRecoveryError::InvalidProjection);
            }
            if delivery.publication.policy.model_visible {
                projection.push(decode_payload(&delivery.projection)?);
            }
        } else if delivery.disposition.is_none() {
            return Err(WorkRecoveryError::InvalidProjection);
        }
    }
    let stage = match work.stage {
        WorkStage::ReasonReady => RecoveredStage::ReasonReady,
        WorkStage::ReasonInFlight => RecoveredStage::ReasonUncertain {
            request_id: work
                .request_id
                .clone()
                .ok_or(WorkRecoveryError::MissingCheckpoint)?,
            request: work
                .reason_request
                .clone()
                .ok_or(WorkRecoveryError::MissingCheckpoint)?,
        },
        WorkStage::ActReady => {
            let response = decode_payload(
                work.response
                    .as_ref()
                    .ok_or(WorkRecoveryError::MissingCheckpoint)?,
            )?;
            let mut prepared = Vec::new();
            let mut settled = Vec::new();
            let mut uncertain = Vec::new();
            for invocation_id in &work.invocation_ids {
                let invocation = snapshot
                    .state
                    .invocations
                    .get(invocation_id)
                    .ok_or(WorkRecoveryError::InvalidInvocation)?;
                if invocation.intent.invocation_id != *invocation_id
                    || invocation.work_id.as_deref() != Some(work_id)
                    || invocation.recipient_lifecycle != batch.recipient_lifecycle
                {
                    return Err(WorkRecoveryError::InvalidInvocation);
                }
                match invocation.status {
                    InvocationStatus::Prepared => prepared.push(invocation.intent.clone()),
                    InvocationStatus::DispatchAccepted | InvocationStatus::OutcomeUnknown => {
                        uncertain.push(invocation_id.clone());
                    }
                    InvocationStatus::Settled => {
                        let outcome = invocation
                            .outcome
                            .as_ref()
                            .ok_or(WorkRecoveryError::InvalidInvocation)?;
                        match outcome {
                            InvocationOutcome::Completed { result }
                            | InvocationOutcome::Failed { result } => {
                                decode_payload(result)?;
                            }
                            InvocationOutcome::Cancelled { evidence } if evidence.is_empty() => {
                                return Err(WorkRecoveryError::InvalidInvocation);
                            }
                            InvocationOutcome::Cancelled { .. } => {}
                        }
                        settled.push(InvocationResult {
                            invocation_id: invocation_id.clone(),
                            outcome: outcome.clone(),
                        });
                    }
                }
            }
            if uncertain.is_empty() {
                RecoveredStage::ActReady {
                    response,
                    prepared,
                    settled,
                }
            } else {
                RecoveredStage::ReconcileInvocations {
                    invocation_ids: uncertain,
                }
            }
        }
        WorkStage::Blocked => RecoveredStage::Blocked {
            reason: work.reason.clone(),
            recovery_condition: work.recovery_condition.clone(),
        },
        WorkStage::Settled | WorkStage::Abandoned => RecoveredStage::Finished,
    };
    Ok(RecoveredWork {
        target: WorkTarget {
            work_id: work.work_id.clone(),
            expected_work_revision: work.revision,
        },
        budget_id: work.budget_id.clone(),
        batch_id: work.batch_id.clone(),
        delivery_ids: batch.delivery_ids.clone(),
        processing_delivery_ids: batch.processing_delivery_ids.clone(),
        projection,
        stage,
    })
}

#[cfg(test)]
#[path = "work_recovery_test.rs"]
mod tests;
