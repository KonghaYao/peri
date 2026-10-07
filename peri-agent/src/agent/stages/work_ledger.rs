use std::sync::Arc;

use peri_acp_types::session_resources::work::{
    WorkAction, WorkCommand, WorkDecision, WorkQuery, WorkReceipt, WorkRejection, WorkResolution,
    WorkSnapshot,
};
use peri_acp_types::session_resources::{MutationOutcome, SessionResourceError, SessionResources};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkCommitError {
    #[error("work mutation was rejected")]
    Rejected { receipt: Box<WorkReceipt> },
    #[error("work mutation outcome is unknown")]
    Unknown { command: Box<WorkCommand> },
    #[error("another work mutation is awaiting resolution")]
    Frozen { command: Box<WorkCommand> },
    #[error("work receipt does not match the original mutation")]
    InvalidReceipt { command: Box<WorkCommand> },
    #[error(transparent)]
    Resource(#[from] SessionResourceError),
}

pub(crate) struct WorkMutationBarrier {
    resources: Arc<dyn SessionResources>,
    unconfirmed: Mutex<Option<WorkCommand>>,
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

    pub(crate) async fn pending_command(&self) -> Option<WorkCommand> {
        self.unconfirmed.lock().await.clone()
    }

    pub(crate) async fn commit(
        &self,
        command: &WorkCommand,
    ) -> Result<WorkReceipt, WorkCommitError> {
        command.digest()?;
        let mut unconfirmed = self.unconfirmed.lock().await;
        if let Some(original) = unconfirmed.as_ref() {
            if original != command {
                return Err(WorkCommitError::Frozen {
                    command: Box::new(original.clone()),
                });
            }
            return self.resolve_original(command, &mut unconfirmed).await;
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
        original: &WorkCommand,
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
                    let guard = match &mut command.action {
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
                    guard.expected_revision = availability.state.revision;
                    command.mutation_id = uuid::Uuid::now_v7().to_string();
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn resolve_original(
        &self,
        command: &WorkCommand,
        unconfirmed: &mut Option<WorkCommand>,
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
                        command: Box::new(command.clone()),
                    }),
                }
            }
            Ok(WorkResolution::Unknown) | Err(_) => Err(WorkCommitError::Unknown {
                command: Box::new(command.clone()),
            }),
        }
    }

    fn confirm(
        command: &WorkCommand,
        receipt: WorkReceipt,
        unconfirmed: &mut Option<WorkCommand>,
    ) -> Result<WorkReceipt, WorkCommitError> {
        if receipt.session_id != command.session_id || receipt.mutation_id != command.mutation_id {
            return Err(WorkCommitError::InvalidReceipt {
                command: Box::new(command.clone()),
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
