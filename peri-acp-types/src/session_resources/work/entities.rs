use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PayloadRef {
    pub storage_scope: String,
    pub payload_id: String,
    pub encoding: u32,
    pub byte_length: u64,
    pub sha256: String,
}

impl PayloadRef {
    pub fn validate(&self) -> SessionResourceResult<()> {
        if self.storage_scope.is_empty()
            || self.payload_id.is_empty()
            || self.encoding == 0
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid("invalid immutable payload reference"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkPayload {
    pub message_id: MessageId,
    pub role: String,
    pub content: PayloadRef,
    pub tool_call_id: Option<String>,
}

impl WorkPayload {
    pub fn validate(&self) -> SessionResourceResult<()> {
        self.content.validate()?;
        if (self.role == "tool"
            && self
                .tool_call_id
                .as_ref()
                .is_none_or(|identity| identity.is_empty()))
            || (self.role != "tool" && self.tool_call_id.is_some())
        {
            return Err(invalid(
                "canonical tool result must carry its exact call identity",
            ));
        }
        if !matches!(
            self.role.as_str(),
            "user" | "assistant" | "tool" | "system" | "system_reminder"
        ) {
            return Err(invalid("invalid canonical message role"));
        }
        Ok(())
    }
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
            max_batch_size: MAX_WORK_PAGE_SIZE.into(),
            reason_requests: DEFAULT_AGENT_MAX_ITERATIONS as u64,
            dispatches: DEFAULT_AGENT_MAX_ITERATIONS as u64 * 4,
            recoveries: 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionWorkHead {
    pub lifecycle: u64,
    pub change_seq: u64,
    pub next_delivery_seq: u64,
    pub publication_generation: u64,
    pub limits: WorkLimits,
    pub required_count: u64,
    pub optional_count: u64,
    pub required_bytes: u64,
    pub optional_bytes: u64,
    pub unresolved_effects: u64,
    pub terminal_obligations: u64,
    pub legacy_unknown: u64,
    pub current_processing_id: Option<String>,
    pub current_admission_id: Option<String>,
    pub recovery_descriptor_id: Option<String>,
}

impl Default for SessionWorkHead {
    fn default() -> Self {
        Self {
            lifecycle: 1,
            change_seq: 0,
            next_delivery_seq: 1,
            publication_generation: 0,
            limits: WorkLimits::default(),
            required_count: 0,
            optional_count: 0,
            required_bytes: 0,
            optional_bytes: 0,
            unresolved_effects: 0,
            terminal_obligations: 0,
            legacy_unknown: 0,
            current_processing_id: None,
            current_admission_id: None,
            recovery_descriptor_id: None,
        }
    }
}

impl SessionWorkHead {
    pub fn has_pending_work(&self) -> bool {
        self.required_count != 0
            || self.unresolved_effects != 0
            || self.terminal_obligations != 0
            || self.legacy_unknown != 0
            || self.current_processing_id.is_some()
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
pub struct Delivery {
    pub delivery_id: String,
    pub recipient_lifecycle: u64,
    pub revision: u64,
    pub publication: PublishDelivery,
    pub admission_sequence: u64,
    pub projection: Option<MessageId>,
    pub projection_version: u64,
    pub processing_id: Option<String>,
    pub batch_ordinal: Option<u32>,
    pub participates_in_reason: bool,
    pub obligation: ObligationStatus,
    pub disposition: Option<String>,
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkBudget {
    pub reason_requests: u64,
    pub dispatches: u64,
    pub recoveries: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationRef {
    pub parent_session_id: String,
    pub parent_lifecycle: u64,
    pub delegation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Processing {
    pub processing_id: String,
    pub recipient_lifecycle: u64,
    pub revision: u64,
    pub execution: ControlAttempt,
    pub stage: WorkStage,
    pub phase_sequence: u64,
    pub delivery_count: u32,
    pub reason_delivery_count: u32,
    pub budget: WorkBudget,
    pub checkpoint: Option<String>,
    pub request_id: Option<String>,
    pub request: Option<PayloadRef>,
    pub response: Option<WorkPayload>,
    pub remaining_effects: u64,
    pub resume_stage: Option<WorkStage>,
    pub blocked_evidence: Option<String>,
    pub recovery_condition: Option<String>,
    pub delegation: Option<DelegationRef>,
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
pub struct Effect {
    pub invocation_id: String,
    pub recipient_lifecycle: u64,
    pub revision: u64,
    pub processing_id: Option<String>,
    pub phase_sequence: u64,
    pub intent: InvocationIntent,
    pub binding: Option<TaskBinding>,
    pub status: InvocationStatus,
    pub outcome: Option<InvocationOutcome>,
    pub unknown_reason: Option<String>,
    pub delegation: Option<DelegationRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StagedUserInputStatus {
    Queued,
    Published,
    Withdrawn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagedUserInput {
    pub input_id: String,
    pub recipient_lifecycle: u64,
    pub revision: u64,
    pub sequence: u64,
    pub publication_generation: u64,
    pub content: PayloadRef,
    pub command_id: String,
    pub fingerprint: u64,
    pub status: StagedUserInputStatus,
    pub publication_id: Option<String>,
    pub withdrawal: Option<(String, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInputPublicationIdentity {
    pub input_id: String,
    pub publication_generation: String,
    pub fingerprint: u64,
    pub command_id: String,
    pub draft_binding: StagedUserInputPublicationBinding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedUserInputPublicationBinding {
    pub draft_revision: u64,
    pub draft_fingerprint: u64,
    pub canonical_content: PayloadRef,
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
    pub entering_mutation_id: String,
    pub leaving_evidence_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceOwnerBinding {
    pub recipient_lifecycle: u64,
    pub connections_json: String,
    pub authorization_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryDescriptor {
    pub descriptor_id: String,
    pub recipient_lifecycle: u64,
    pub revision: u64,
    pub resource_owners: Option<ResourceOwnerBinding>,
    pub child_resume_metadata_json: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalObligation {
    pub admission_id: String,
    pub command: Box<WorkCommand>,
    pub acknowledgement: Option<WorkReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyEvidence {
    pub record_id: String,
    pub evidence: String,
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
    pub requires_recovery: bool,
    pub delivery_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkAvailability {
    pub lifecycle: u64,
    pub change_seq: u64,
    pub blocked: bool,
    pub pending: bool,
    pub candidates: Vec<WorkCandidate>,
}
