use std::sync::Arc;

use peri_acp_types::session_resources::work::{
    WorkCommand, WorkDecision, WorkQuery, WorkReceipt, WorkResolution, WorkSnapshot,
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
