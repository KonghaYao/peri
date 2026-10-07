use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::session_resources::{
    work::{WorkAdmission, WorkCandidate, WorkSnapshot},
    ControlAttempt, ControlState, SessionResourceResult,
};

pub const ADMIT_METHOD: &str = "peri/execution/admit";
pub const SETTLE_METHOD: &str = "peri/execution/settle";
pub const ENTERED_METHOD: &str = "peri/execution/entered";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionSnapshot {
    pub session_id: String,
    pub control: ControlState,
    pub candidates: Vec<WorkCandidate>,
    pub blocked: bool,
}

impl From<&WorkSnapshot> for AdmissionSnapshot {
    fn from(snapshot: &WorkSnapshot) -> Self {
        Self {
            session_id: snapshot.session_id.clone(),
            control: snapshot.control.clone(),
            candidates: snapshot.candidates.clone(),
            blocked: snapshot.blocked,
        }
    }
}

impl From<WorkSnapshot> for AdmissionSnapshot {
    fn from(snapshot: WorkSnapshot) -> Self {
        Self {
            session_id: snapshot.session_id,
            control: snapshot.control,
            candidates: snapshot.candidates,
            blocked: snapshot.blocked,
        }
    }
}

impl AdmissionSnapshot {
    pub fn validate_admission(&self, admission: &WorkAdmission) -> SessionResourceResult<()> {
        crate::session_resources::work::validate_admission_association(
            &self.session_id,
            &self.control,
            &self.candidates,
            self.blocked,
            admission,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryRequest {
    pub admission: WorkAdmission,
    pub entry_evidence_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryReceipt {
    pub admission: WorkAdmission,
    pub entry_evidence_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum EntryOutcome {
    Applied { receipt: EntryReceipt },
    Blocked { reason: String },
    Unknown,
    NotApplied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionRequest {
    pub request_id: String,
    pub snapshot: AdmissionSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_admission: Option<WorkAdmission>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum AdmissionOutcome {
    Admitted { admission: WorkAdmission },
    Busy,
    Blocked { reason: String },
    Unknown,
    NotApplied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum AttemptStoppedProof {
    #[serde(rename_all = "camelCase")]
    AttemptStopped {
        instance_id: String,
        generation_id: String,
        execution: ControlAttempt,
        evidence_id: String,
    },
}

impl AttemptStoppedProof {
    pub fn validates(&self, admission: &WorkAdmission) -> bool {
        let Self::AttemptStopped {
            instance_id,
            generation_id,
            execution,
            evidence_id,
        } = self;
        instance_id == &admission.instance_id
            && generation_id == &admission.generation_id
            && execution == &admission.execution
            && !evidence_id.is_empty()
    }

    pub fn evidence_id(&self) -> &str {
        let Self::AttemptStopped { evidence_id, .. } = self;
        evidence_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettlementRequest {
    pub admission: WorkAdmission,
    pub proof: AttemptStoppedProof,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettlementReceipt {
    pub admission: WorkAdmission,
    pub evidence_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum SettlementOutcome {
    Applied { receipt: SettlementReceipt },
    Rejected { reason: String },
    Unknown,
    NotApplied,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionAdmissionError {
    #[error("SDK execution admission capability unavailable")]
    Unavailable,
    #[error("invalid SDK execution admission protocol: {0}")]
    Protocol(String),
}

#[async_trait]
pub trait ExecutionAdmissionPort: Send + Sync {
    async fn entered(&self, request: EntryRequest)
        -> Result<EntryOutcome, ExecutionAdmissionError>;
    async fn admit(
        &self,
        request: AdmissionRequest,
    ) -> Result<AdmissionOutcome, ExecutionAdmissionError>;
    async fn settle(
        &self,
        request: SettlementRequest,
    ) -> Result<SettlementOutcome, ExecutionAdmissionError>;
}

#[cfg(test)]
#[path = "execution_admission_test.rs"]
mod tests;
