use std::sync::Arc;

use peri_acp_types::session_resources::work::{
    PreparedWorkCommand, WorkAction, WorkDecision, WorkQuery, WorkReceipt, WorkRejection,
    WorkResolution, WorkSnapshot,
};
use peri_acp_types::session_resources::{MutationOutcome, SessionResourceError, SessionResources};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkCommitError {
    #[error("work mutation was rejected")]
    Rejected { receipt: Box<WorkReceipt> },
    #[error("work mutation outcome is unknown")]
    Unknown { command: PreparedWorkCommand },
    #[error("another work mutation is awaiting resolution")]
    Frozen { command: PreparedWorkCommand },
    #[error("work receipt does not match the original mutation")]
    InvalidReceipt { command: PreparedWorkCommand },
    #[error(transparent)]
    Resource(#[from] SessionResourceError),
}

pub(crate) struct WorkMutationBarrier {
    resources: Arc<dyn SessionResources>,
    unconfirmed: Mutex<Option<PreparedWorkCommand>>,
}

impl WorkMutationBarrier {
    pub(crate) fn resources(&self) -> Arc<dyn SessionResources> {
        Arc::clone(&self.resources)
    }
    pub(crate) fn new(resources: Arc<dyn SessionResources>) -> Self {
        Self {
            resources,
            unconfirmed: Mutex::new(None),
        }
    }

    pub(crate) async fn snapshot(
        &self,
        query: &WorkQuery,
    ) -> Result<WorkSnapshot, WorkCommitError> {
        let snapshot = self.resources.load_session_work(query).await?;
        if snapshot.session_id != query.session_id {
            return Err(SessionResourceError::conflict("work snapshot session mismatch").into());
        }
        Ok(snapshot)
    }

    pub(crate) async fn pending_command(&self) -> Option<PreparedWorkCommand> {
        self.unconfirmed.lock().await.clone()
    }

    pub(crate) async fn commit(
        &self,
        command: &PreparedWorkCommand,
    ) -> Result<WorkReceipt, WorkCommitError> {
        let mut unconfirmed = self.unconfirmed.lock().await;
        if let Some(original) = unconfirmed.as_ref() {
            if original != command {
                return Err(WorkCommitError::Frozen {
                    command: original.clone(),
                });
            }
            let original = original.clone();
            return self.resolve_original(&original, &mut unconfirmed).await;
        }
        *unconfirmed = Some(command.clone());
        match self.resources.apply_work_mutation(command).await {
            Ok(receipt) => Self::confirm(command, receipt, &mut unconfirmed),
            Err(error) if error.effect() == MutationOutcome::NotApplied => {
                *unconfirmed = None;
                Err(error.into())
            }
            Err(_) => self.resolve_original(command, &mut unconfirmed).await,
        }
    }

    pub(crate) async fn commit_execution_transition(
        &self,
        original: &PreparedWorkCommand,
    ) -> Result<WorkReceipt, WorkCommitError> {
        let mut command = original.clone();
        for attempt in 0..8 {
            match self.commit(&command).await {
                Err(WorkCommitError::Rejected { receipt })
                    if attempt < 7
                        && receipt.decision
                            == (WorkDecision::Rejected {
                                reason: WorkRejection::StaleRevision,
                            }) =>
                {
                    let availability = self
                        .resources
                        .load_work_availability(&command.session_id)
                        .await?;
                    let settlement_only = matches!(
                        &command.action,
                        WorkAction::CommitAct {
                            next_work_id: None,
                            ..
                        }
                    );
                    let guard = match &command.action {
                        WorkAction::ClaimBatch { guard, .. }
                        | WorkAction::BeginReason { guard, .. }
                        | WorkAction::CommitReasonResponseAndDispatchIntent { guard, .. }
                        | WorkAction::BeginDispatch { guard, .. }
                        | WorkAction::CommitAct { guard, .. } => guard,
                        _ => return Err(WorkCommitError::Rejected { receipt }),
                    };
                    if availability.control.lifecycle != command.recipient_lifecycle
                        || (!settlement_only
                            && (availability.control.control_generation
                                != guard.expected_control_generation
                                || availability.control.attempt.as_ref() != Some(&guard.execution)))
                    {
                        return Err(WorkCommitError::Rejected { receipt });
                    }
                    let mut raw = command.into_command();
                    let guard = match &mut raw.action {
                        WorkAction::ClaimBatch { guard, .. }
                        | WorkAction::BeginReason { guard, .. }
                        | WorkAction::CommitReasonResponseAndDispatchIntent { guard, .. }
                        | WorkAction::BeginDispatch { guard, .. }
                        | WorkAction::CommitAct { guard, .. } => guard,
                        _ => unreachable!(),
                    };
                    guard.expected_revision = availability.state.revision;
                    raw.mutation_id = uuid::Uuid::now_v7().to_string();
                    command = PreparedWorkCommand::try_new(raw)?;
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn resolve_original(
        &self,
        command: &PreparedWorkCommand,
        unconfirmed: &mut Option<PreparedWorkCommand>,
    ) -> Result<WorkReceipt, WorkCommitError> {
        match self.resources.resolve_work_mutation(command).await {
            Ok(WorkResolution::Applied { receipt }) => Self::confirm(command, receipt, unconfirmed),
            Ok(WorkResolution::NotApplied) => {
                match self.resources.apply_work_mutation(command).await {
                    Ok(receipt) => Self::confirm(command, receipt, unconfirmed),
                    Err(error) if error.effect() == MutationOutcome::NotApplied => {
                        *unconfirmed = None;
                        Err(error.into())
                    }
                    Err(_) => Err(WorkCommitError::Unknown {
                        command: command.clone(),
                    }),
                }
            }
            Ok(WorkResolution::Unknown) | Err(_) => Err(WorkCommitError::Unknown {
                command: command.clone(),
            }),
        }
    }

    fn confirm(
        command: &PreparedWorkCommand,
        receipt: WorkReceipt,
        unconfirmed: &mut Option<PreparedWorkCommand>,
    ) -> Result<WorkReceipt, WorkCommitError> {
        if receipt.session_id != command.session_id || receipt.mutation_id != command.mutation_id {
            return Err(WorkCommitError::InvalidReceipt {
                command: command.clone(),
            });
        }
        *unconfirmed = None;
        match receipt.decision {
            WorkDecision::Accepted => Ok(receipt),
            WorkDecision::Rejected { .. } => Err(WorkCommitError::Rejected {
                receipt: Box::new(receipt),
            }),
        }
    }
}

#[cfg(test)]
#[path = "work_ledger_test.rs"]
mod tests;
