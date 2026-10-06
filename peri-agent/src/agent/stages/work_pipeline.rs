use std::sync::Arc;

use peri_acp_types::session_resources::work::{
    ReasonRequest, WorkAction, WorkAdmission, WorkCommand, WorkGuard, WorkInspection,
};
use peri_acp_types::session_resources::SessionResources;
use sha2::{Digest, Sha256};

use super::work_ledger::WorkMutationBarrier;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkMode {
    #[default]
    Required,
    #[cfg(test)]
    BestEffortFixture,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkSetupError {
    #[error("required work execution has no SDK admission ticket")]
    MissingAdmission,
    #[error("required work execution has no durable SessionResources port")]
    MissingResources,
    #[error("work execution admission identity is incomplete")]
    InvalidAdmission,
    #[error("work snapshot belongs to a different session")]
    SessionMismatch,
    #[error("model request checkpoint identity is incomplete")]
    InvalidRequest,
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

pub(crate) enum WorkRuntime {
    Durable(Arc<WorkSession>),
    #[cfg(test)]
    BestEffortFixture,
}

pub(crate) struct WorkSession {
    pub(crate) admission: WorkAdmission,
    pub(crate) ledger: WorkMutationBarrier,
}

impl WorkRuntime {
    pub(crate) fn bind(
        mode: WorkMode,
        admission: Option<&WorkAdmission>,
        resources: Option<Arc<dyn SessionResources>>,
    ) -> Result<Self, WorkSetupError> {
        #[cfg(test)]
        if mode == WorkMode::BestEffortFixture {
            return Ok(Self::BestEffortFixture);
        }
        let _ = mode;
        let admission = admission.ok_or(WorkSetupError::MissingAdmission)?;
        if admission.session_id.is_empty()
            || admission.admission_id.is_empty()
            || admission.work_id.is_empty()
            || admission.lifecycle == 0
        {
            return Err(WorkSetupError::InvalidAdmission);
        }
        let resources = resources.ok_or(WorkSetupError::MissingResources)?;
        Ok(Self::Durable(Arc::new(WorkSession {
            admission: admission.clone(),
            ledger: WorkMutationBarrier::new(resources),
        })))
    }
}

impl WorkSession {
    pub(crate) fn guard(&self, snapshot: &WorkInspection) -> Result<WorkGuard, WorkSetupError> {
        if snapshot.session_id != self.admission.session_id {
            return Err(WorkSetupError::SessionMismatch);
        }
        Ok(WorkGuard {
            expected_revision: snapshot.head.change_seq,
            expected_control_generation: self.admission.control_generation,
            execution: self.admission.execution.clone(),
        })
    }

    pub(crate) fn command(&self, action: WorkAction) -> WorkCommand {
        WorkCommand {
            session_id: self.admission.session_id.clone(),
            recipient_lifecycle: self.admission.lifecycle,
            mutation_id: uuid::Uuid::now_v7().to_string(),
            action,
        }
    }
}

pub(crate) async fn request_checkpoint(
    session: &WorkSession,
    request: &serde_json::Value,
    model_ref: String,
    authorization_ref: String,
) -> anyhow::Result<ReasonRequest> {
    if model_ref.is_empty() || authorization_ref.is_empty() {
        return Err(WorkSetupError::InvalidRequest.into());
    }
    let serialized_request = serde_json::to_string(request)?;
    let request_digest = format!("{:x}", Sha256::digest(serialized_request.as_bytes()));
    Ok(ReasonRequest {
        payload: super::work_reads::prepare_evidence(
            session.ledger.resources().as_ref(),
            &session.admission.session_id,
            serialized_request.into_bytes(),
        )
        .await?,
        request_digest,
        model_ref,
        authorization_ref,
    })
}

#[cfg(test)]
#[path = "work_pipeline_test.rs"]
mod tests;
