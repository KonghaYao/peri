use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::messages::MessageId;
use crate::session::MessagePolicy;
use crate::session_resources::{
    ControlAttempt, ControlState, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult,
};
use crate::store::{deserialize_persisted_payload, serialize_persisted_payload, PersistedPayload};

#[path = "work/availability.rs"]
mod availability;
pub use availability::{WorkAvailability, WorkAvailabilityState};

#[path = "work/admission.rs"]
mod admission;
#[path = "work/bindings.rs"]
mod bindings;
#[path = "work/delivery.rs"]
mod delivery;
#[path = "work/policy.rs"]
mod policy;
#[path = "work/processing.rs"]
mod processing;
#[path = "work/projection.rs"]
mod projection;
#[path = "work/query.rs"]
mod query;
#[path = "work/reducer.rs"]
mod reducer;
#[path = "work/user_input.rs"]
mod user_input;

pub use policy::DEFAULT_AGENT_MAX_ITERATIONS;
pub use user_input::{StagedUserInput, StagedUserInputStatus};

pub use query::validate_admission_association;
pub use reducer::{reduce_work, WorkReduction};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkPayload {
    pub message_id: MessageId,
    pub role: String,
    pub serialized: String,
}

impl WorkPayload {
    pub fn with_message_id(&self, message_id: MessageId) -> SessionResourceResult<Self> {
        let mut payload = deserialize_persisted_payload(&self.serialized)
            .map_err(|_| invalid("invalid canonical work payload"))?;
        match &mut payload {
            PersistedPayload::Message(message) => match message {
                crate::messages::BaseMessage::Human { id, .. }
                | crate::messages::BaseMessage::Ai { id, .. }
                | crate::messages::BaseMessage::Tool { id, .. }
                | crate::messages::BaseMessage::System { id, .. } => *id = message_id,
            },
            PersistedPayload::SystemReminder { id, .. } => *id = message_id,
        }
        Self::from_payload(&payload)
    }
    pub fn from_payload(payload: &PersistedPayload) -> SessionResourceResult<Self> {
        let role = match payload {
            PersistedPayload::SystemReminder { .. } => "system_reminder",
            PersistedPayload::Message(message) => match message {
                crate::messages::BaseMessage::Human { .. } => "user",
                crate::messages::BaseMessage::Ai { .. } => "assistant",
                crate::messages::BaseMessage::Tool { .. } => "tool",
                crate::messages::BaseMessage::System { .. } => "system",
            },
        };
        Ok(Self {
            message_id: payload.id(),
            role: role.into(),
            serialized: serialize_persisted_payload(payload)
                .map_err(|_| invalid("invalid canonical work payload"))?,
        })
    }

    pub fn validate(&self) -> SessionResourceResult<()> {
        let payload = deserialize_persisted_payload(&self.serialized)
            .map_err(|_| invalid("invalid canonical work payload"))?;
        if Self::from_payload(&payload)? != *self {
            return Err(invalid("canonical work payload identity or role mismatch"));
        }
        Ok(())
    }
}

pub(super) fn invalid(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.into(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkEvent {
    pub producer_namespace: String,
    pub event_id: String,
    pub event_kind: String,
    pub causation_id: Option<String>,
    pub content: WorkPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryPurpose {
    UserInput,
    TaskTerminal,
    Continuation,
    Observation,
    Settlement,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishDelivery {
    pub delivery_id: String,
    pub event: WorkEvent,
    pub purpose: DeliveryPurpose,
    pub policy: MessagePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkGuard {
    pub expected_revision: u64,
    pub expected_control_generation: u64,
    pub execution: ControlAttempt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkTarget {
    pub work_id: String,
    pub expected_work_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasonRequest {
    pub serialized_request: String,
    pub request_digest: String,
    pub model_ref: String,
    pub authorization_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationIntent {
    pub invocation_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub arguments_digest: String,
    pub effective_tool_name: String,
    pub effective_arguments_json: String,
    pub effective_arguments_digest: String,
    pub owner_identity: String,
    pub scope_id: String,
    pub scope_epoch: Option<u64>,
    pub authorization_ref: String,
    pub recovery_locator: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum InvocationOutcome {
    Completed { result: WorkPayload },
    Failed { result: WorkPayload },
    Cancelled { evidence: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationResult {
    pub invocation_id: String,
    pub outcome: InvocationOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskBinding {
    pub invocation_id: String,
    pub owner_identity: String,
    pub owner_task_id: String,
    pub initiator_session_id: String,
    pub recipient_lifecycle: u64,
    pub recovery_locator: String,
    pub authorization_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum WorkAction {
    StageUserInput {
        input_json: String,
        command_id: String,
        fingerprint: u64,
    },
    WithdrawStagedUserInput {
        input_id: String,
        command_id: String,
        fingerprint: u64,
    },
    PublishStagedUserInputs {
        expected_revision: u64,
        expected_control_generation: u64,
        expected_attempt: Option<ControlAttempt>,
        interrupt_current: bool,
        deliveries: Vec<PublishDelivery>,
    },
    RegisterAdmission {
        admission: WorkAdmission,
    },
    FinishAdmission {
        admission: WorkAdmission,
        evidence_id: String,
    },
    BindResourceOwners {
        expected_revision: u64,
        connections_json: String,
        authorization_ref: String,
    },
    BindChildResumeMetadata {
        expected_revision: u64,
        metadata_json: String,
    },
    BindTerminalObligation {
        expected_revision: u64,
        admission_id: String,
        command: Box<WorkCommand>,
    },
    BindWorkDelegation {
        expected_revision: u64,
        work_id: String,
        binding: TaskBinding,
        parent_binding_receipt: WorkReceipt,
    },
    AcknowledgeTerminalObligation {
        expected_revision: u64,
        admission_id: String,
        receipt: WorkReceipt,
    },
    PublishDelivery {
        delivery: PublishDelivery,
    },
    PublishTaskSettlement {
        delivery: PublishDelivery,
        binding: TaskBinding,
    },
    ClaimBatch {
        guard: WorkGuard,
        batch_id: String,
        delivery_ids: Vec<String>,
    },
    BeginReason {
        guard: WorkGuard,
        target: WorkTarget,
        request_id: String,
        request: ReasonRequest,
    },
    CommitReasonResponseAndDispatchIntent {
        guard: WorkGuard,
        target: WorkTarget,
        request_id: String,
        response: WorkPayload,
        dispatch_intents: Vec<InvocationIntent>,
        next_work_id: Option<String>,
    },
    BeginDispatch {
        guard: WorkGuard,
        target: WorkTarget,
        invocation_id: String,
    },
    OutcomeUnknown {
        expected_revision: u64,
        target: WorkTarget,
        invocation_id: String,
        reason: String,
    },
    CommitAct {
        guard: WorkGuard,
        target: WorkTarget,
        results: Vec<InvocationResult>,
        next_work_id: Option<String>,
    },
    BlockWork {
        expected_revision: u64,
        target: WorkTarget,
        reason: String,
        recovery_condition: String,
    },
    ResumeWork {
        guard: WorkGuard,
        target: WorkTarget,
        recovery_evidence: String,
    },
    AbandonWork {
        expected_revision: u64,
        expected_control_generation: u64,
        target: WorkTarget,
        reason: String,
        authorization_ref: String,
    },
    SettleWork {
        expected_revision: u64,
        target: WorkTarget,
    },
    PrepareInvocation {
        expected_revision: u64,
        intent: InvocationIntent,
    },
    ReconcileTaskBinding {
        /// Observed session revision; future revisions are rejected, older revisions
        /// do not invalidate the immutable per-invocation binding comparison.
        expected_revision: u64,
        binding: TaskBinding,
    },
    WithdrawDelivery {
        expected_revision: u64,
        #[serde(default)]
        expected_control_generation: Option<u64>,
        delivery_id: String,
        authorization_ref: String,
    },
    AbandonDelivery {
        guard: WorkGuard,
        delivery_id: String,
        reason: String,
        evidence: String,
    },
    ResetBudget {
        expected_revision: u64,
        budget_id: String,
        authorization_ref: String,
    },
    QuarantineLegacy {
        expected_revision: u64,
        record_id: String,
        evidence: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkCommand {
    pub session_id: String,
    pub recipient_lifecycle: u64,
    pub mutation_id: String,
    pub action: WorkAction,
}

impl WorkCommand {
    pub fn digest(&self) -> SessionResourceResult<String> {
        if self.session_id.is_empty()
            || self.mutation_id.is_empty()
            || self.mutation_id.len() > 256
            || self.recipient_lifecycle == 0
        {
            return Err(invalid("invalid work mutation identity"));
        }
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("invalid work command"))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkLimits {
    pub required_deliveries: u64,
    pub optional_deliveries: u64,
    pub required_bytes: u64,
    pub optional_bytes: u64,
    pub max_batch_size: u64,
    pub reason_requests: u64,
    pub dispatches: u64,
    pub recoveries: u64,
}

impl Default for WorkLimits {
    fn default() -> Self {
        Self {
            required_deliveries: 1024,
            optional_deliveries: 128,
            required_bytes: 8 * 1024 * 1024,
            optional_bytes: 512 * 1024,
            max_batch_size: 64,
            reason_requests: DEFAULT_AGENT_MAX_ITERATIONS as u64,
            dispatches: policy::DEFAULT_DISPATCH_BUDGET,
            recoveries: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObligationStatus {
    Pending,
    InProgress,
    Satisfied,
    Blocked,
    Suppressed,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryRecord {
    pub recipient_lifecycle: u64,
    pub publication: PublishDelivery,
    pub projection: WorkPayload,
    pub admission_sequence: u64,
    pub projected: bool,
    pub projection_version: u64,
    pub batch_id: Option<String>,
    pub disposition: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequiredObligation {
    pub delivery_id: String,
    pub status: ObligationStatus,
    pub work_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessingBatch {
    pub batch_id: String,
    pub delivery_ids: Vec<String>,
    pub processing_delivery_ids: Vec<String>,
    pub projection_versions: BTreeMap<String, u64>,
    pub execution: ControlAttempt,
    pub recipient_lifecycle: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkStage {
    ReasonReady,
    ReasonInFlight,
    ActReady,
    Settled,
    Blocked,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkRecord {
    pub work_id: String,
    pub revision: u64,
    pub budget_id: String,
    pub batch_id: String,
    pub stage: WorkStage,
    pub resume_stage: Option<WorkStage>,
    pub request_id: Option<String>,
    pub reason_request: Option<ReasonRequest>,
    pub response: Option<WorkPayload>,
    pub invocation_ids: Vec<String>,
    pub reason: Option<String>,
    pub recovery_condition: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InvocationStatus {
    Prepared,
    DispatchAccepted,
    OutcomeUnknown,
    Settled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationRecord {
    pub intent: InvocationIntent,
    pub recipient_lifecycle: u64,
    pub work_id: Option<String>,
    pub status: InvocationStatus,
    pub outcome: Option<InvocationOutcome>,
    pub unknown_reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkBudget {
    pub reason_requests: u64,
    pub dispatches: u64,
    pub recoveries: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkState {
    pub revision: u64,
    pub next_admission_sequence: u64,
    pub limits: WorkLimits,
    pub deliveries: BTreeMap<String, DeliveryRecord>,
    pub obligations: BTreeMap<String, RequiredObligation>,
    pub batches: BTreeMap<String, ProcessingBatch>,
    pub works: BTreeMap<String, WorkRecord>,
    pub budgets: BTreeMap<String, WorkBudget>,
    pub invocations: BTreeMap<String, InvocationRecord>,
    pub task_bindings: BTreeMap<String, TaskBinding>,
    pub legacy_unknown: BTreeMap<String, String>,
    pub admissions: BTreeMap<String, AdmissionRecord>,
    pub resource_owners: BTreeMap<u64, ResourceOwnerBinding>,
    #[serde(default)]
    pub child_resume_metadata: BTreeMap<u64, String>,
    #[serde(default)]
    pub terminal_obligations: BTreeMap<String, WorkCommand>,
    #[serde(default)]
    pub terminal_acknowledgements: BTreeMap<String, WorkReceipt>,
    #[serde(default)]
    pub work_delegations: BTreeMap<String, TaskBinding>,
    #[serde(default)]
    pub staged_user_inputs: BTreeMap<String, StagedUserInput>,
    #[serde(default)]
    pub user_input_publications: BTreeMap<String, WorkCommand>,
}

impl Default for WorkState {
    fn default() -> Self {
        Self::new(WorkLimits::default())
    }
}

impl WorkState {
    pub fn new(limits: WorkLimits) -> Self {
        Self {
            revision: 0,
            next_admission_sequence: 1,
            limits,
            deliveries: BTreeMap::new(),
            obligations: BTreeMap::new(),
            batches: BTreeMap::new(),
            works: BTreeMap::new(),
            budgets: BTreeMap::new(),
            invocations: BTreeMap::new(),
            task_bindings: BTreeMap::new(),
            legacy_unknown: BTreeMap::new(),
            admissions: BTreeMap::new(),
            resource_owners: BTreeMap::new(),
            child_resume_metadata: BTreeMap::new(),
            terminal_obligations: BTreeMap::new(),
            terminal_acknowledgements: BTreeMap::new(),
            work_delegations: BTreeMap::new(),
            staged_user_inputs: BTreeMap::new(),
            user_input_publications: BTreeMap::new(),
        }
    }
    pub fn has_pending_work(&self) -> bool {
        self.has_pending_terminal_obligations()
            || self.obligations.values().any(|record| {
                matches!(
                    record.status,
                    ObligationStatus::Pending
                        | ObligationStatus::InProgress
                        | ObligationStatus::Blocked
                )
            })
            || self
                .works
                .values()
                .any(|work| !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned))
    }

    pub fn has_pending_terminal_obligations(&self) -> bool {
        self.terminal_obligations
            .keys()
            .any(|admission_id| !self.terminal_acknowledgements.contains_key(admission_id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkRejection {
    StaleLifecycle,
    StaleRevision,
    StaleControlGeneration,
    StaleExecution,
    StaleWorkRevision,
    Conflict,
    InvalidTransition,
    Capacity,
    VersionExhausted,
    LegacyUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkDecision {
    Accepted,
    Rejected { reason: WorkRejection },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkReceipt {
    pub session_id: String,
    pub mutation_id: String,
    pub before_revision: u64,
    pub revision: u64,
    pub decision: WorkDecision,
    pub delivery_id: Option<String>,
    pub admission_sequence: Option<u64>,
    pub batch_id: Option<String>,
    pub work_id: Option<String>,
    pub work_revision: Option<u64>,
    pub stage: Option<WorkStage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WorkResolution {
    Applied { receipt: WorkReceipt },
    NotApplied,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkQuery {
    pub session_id: String,
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkDeliveryQuery {
    pub session_id: String,
    pub delivery_id: String,
}

#[cfg(test)]
#[path = "work/delivery_query_test.rs"]
mod delivery_query_tests;

#[cfg(test)]
#[path = "work/delivery_policy_test.rs"]
mod delivery_policy_tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkCommandQuery {
    pub session_id: String,
    pub mutation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnedWorkCommand {
    pub command: WorkCommand,
    pub resolution: Option<WorkResolution>,
    pub pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkCandidate {
    pub work_id: String,
    pub work_revision: u64,
    pub stage: WorkStage,
    pub batch_id: Option<String>,
    pub delivery_ids: Vec<String>,
    pub requires_recovery: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkAdmission {
    pub session_id: String,
    pub admission_id: String,
    pub instance_id: String,
    pub generation_id: String,
    pub lifecycle: u64,
    pub control_generation: u64,
    pub work_id: String,
    pub work_revision: u64,
    pub execution: ControlAttempt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionRecord {
    pub admission: WorkAdmission,
    pub entering_receipt: Option<WorkReceipt>,
    pub settled_receipt: Option<WorkReceipt>,
    pub evidence_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceOwnerBinding {
    pub recipient_lifecycle: u64,
    pub connections_json: String,
    pub authorization_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceOwnerFacts {
    pub control: ControlState,
    pub revision: u64,
    pub current_owner: Option<ResourceOwnerBinding>,
    pub previous_owner: Option<ResourceOwnerBinding>,
    pub current_child_metadata: Option<String>,
    pub previous_child_metadata: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkSnapshot {
    pub session_id: String,
    pub control: ControlState,
    pub state: WorkState,
    pub candidates: Vec<WorkCandidate>,
    pub blocked: bool,
    #[serde(default)]
    pub pending_commands: Vec<WorkCommand>,
}
