use super::*;

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
    pub payload: PayloadRef,
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
    pub arguments: PayloadRef,
    pub arguments_digest: String,
    pub effective_tool_name: String,
    pub effective_arguments: PayloadRef,
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
    Completed {
        result: WorkPayload,
    },
    Failed {
        result: WorkPayload,
    },
    Cancelled {
        evidence: String,
        result: WorkPayload,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationResult {
    pub invocation_id: String,
    pub expected_effect_revision: u64,
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
        input_id: String,
        content: PayloadRef,
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
        expected_effect_revision: u64,
    },
    OutcomeUnknown {
        expected_revision: u64,
        target: WorkTarget,
        invocation_id: String,
        expected_effect_revision: u64,
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
