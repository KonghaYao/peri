use std::sync::Arc;

use peri_acp_types::session_resources::work::{
    EvidenceQuery, EvidenceRecord, WorkCommand, WorkDecision, WorkInspection, WorkQuery,
    WorkReceipt, WorkResolution,
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
    #[error("original work mutation was finally not applied")]
    NotApplied { command: Box<WorkCommand> },
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

    pub(crate) async fn inspect(
        &self,
        query: &WorkQuery,
    ) -> Result<WorkInspection, WorkCommitError> {
        let inspection = self.resources.inspect_work(query).await?;
        if inspection.session_id != query.session_id {
            return Err(SessionResourceError::conflict("work inspection session mismatch").into());
        }
        Ok(inspection)
    }

    pub(crate) async fn read_evidence(
        &self,
        query: &EvidenceQuery,
    ) -> Result<EvidenceRecord, WorkCommitError> {
        Ok(self.resources.read_evidence(query).await?)
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
        self.commit(original).await
    }

    async fn resolve_original(
        &self,
        command: &WorkCommand,
        unconfirmed: &mut Option<WorkCommand>,
    ) -> Result<WorkReceipt, WorkCommitError> {
        match self.resources.resolve_work_mutation(command).await {
            Ok(WorkResolution::Applied { receipt }) => Self::confirm(command, receipt, unconfirmed),
            Ok(WorkResolution::NotApplied) => {
                *unconfirmed = None;
                Err(WorkCommitError::NotApplied { command: Box::new(command.clone()) })
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
