use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum WorkSelector {
    Head,
    Availability,
    Inbox,
    Delivery {
        delivery_id: String,
    },
    Processing {
        processing_id: String,
    },
    CurrentProcessing,
    ActiveProcessing,
    ProcessingDeliveries {
        processing_id: String,
    },
    Effects {
        processing_id: String,
        phase_sequence: Option<u64>,
    },
    EffectsById {
        invocation_ids: Vec<String>,
    },
    Effect {
        invocation_id: String,
    },
    TaskBinding {
        owner_identity: String,
        owner_task_id: String,
    },
    TaskBindingByTask {
        owner_task_id: String,
    },
    Drafts,
    UnresolvedInputs,
    Draft {
        input_id: String,
    },
    CurrentAdmission,
    Admission {
        admission_id: String,
    },
    Delegation {
        parent_session_id: String,
        parent_lifecycle: u64,
        delegation_id: String,
    },
    RecoveryDescriptor {
        lifecycle: u64,
    },
    TerminalCommands,
    TerminalCommand {
        admission_id: String,
    },
    Command {
        mutation_id: String,
    },
    PendingCommands,
    LegacyEvidence {
        record_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkQuery {
    pub session_id: String,
    pub selector: WorkSelector,
    pub limit: u32,
    pub cursor: Option<String>,
}

impl WorkQuery {
    pub fn new(session_id: impl Into<String>, selector: WorkSelector) -> Self {
        Self {
            session_id: session_id.into(),
            selector,
            limit: MAX_WORK_PAGE_SIZE,
            cursor: None,
        }
    }

    pub fn validate(&self) -> SessionResourceResult<()> {
        if self.session_id.is_empty() || self.limit == 0 || self.limit > MAX_WORK_PAGE_SIZE {
            return Err(invalid("invalid bounded work query"));
        }
        if let WorkSelector::EffectsById { invocation_ids } = &self.selector {
            if invocation_ids.is_empty()
                || invocation_ids.len() > self.limit as usize
                || invocation_ids.iter().any(|identity| identity.is_empty())
            {
                return Err(invalid("exact effects query exceeds its declared bound"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "records", rename_all = "camelCase")]
pub enum WorkPage {
    Head,
    Availability(WorkAvailability),
    Deliveries(Vec<Delivery>),
    Processings(Vec<Processing>),
    Effects(Vec<Effect>),
    Drafts(Vec<StagedUserInput>),
    Admissions(Vec<AdmissionRecord>),
    RecoveryDescriptors(Vec<RecoveryDescriptor>),
    TerminalCommands(Vec<TerminalObligation>),
    Commands(Vec<OwnedWorkCommand>),
    LegacyEvidence(Vec<LegacyEvidence>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkInspection {
    pub session_id: String,
    pub control: ControlState,
    pub head: SessionWorkHead,
    pub page: WorkPage,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceQuery {
    pub session_id: String,
    pub reference: PayloadRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRecord {
    pub reference: PayloadRef,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceWrite {
    pub session_id: String,
    pub storage_scope: String,
    pub payload_id: String,
    pub encoding: u32,
    pub bytes: Vec<u8>,
}

impl EvidenceWrite {
    pub fn reference(&self) -> SessionResourceResult<PayloadRef> {
        let reference = PayloadRef {
            storage_scope: self.storage_scope.clone(),
            payload_id: self.payload_id.clone(),
            encoding: self.encoding,
            byte_length: self.bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&self.bytes)),
        };
        reference.validate()?;
        if self.session_id.is_empty() {
            return Err(invalid("missing evidence session"));
        }
        Ok(reference)
    }
}

impl EvidenceRecord {
    pub fn validate(&self) -> SessionResourceResult<()> {
        self.reference.validate()?;
        if self.bytes.len() as u64 != self.reference.byte_length
            || format!("{:x}", Sha256::digest(&self.bytes)) != self.reference.sha256
        {
            return Err(invalid("immutable evidence length or digest mismatch"));
        }
        Ok(())
    }
}

pub fn validate_execution_association(admission: &WorkAdmission, control: &ControlState) -> bool {
    admission.lifecycle == control.lifecycle
        && admission.control_generation == control.control_generation
        && control
            .attempt
            .as_ref()
            .is_none_or(|execution| execution == &admission.execution)
        && !admission.admission_id.is_empty()
        && !admission.instance_id.is_empty()
        && !admission.generation_id.is_empty()
}

pub fn validate_admission_association(
    session_id: &str,
    control: &ControlState,
    candidates: &[WorkCandidate],
    blocked: bool,
    admission: &WorkAdmission,
) -> SessionResourceResult<()> {
    if blocked
        || admission.session_id != session_id
        || !validate_execution_association(admission, control)
        || !candidates.iter().any(|candidate| {
            candidate.work_id == admission.work_id
                && candidate.work_revision == admission.work_revision
        })
    {
        return Err(invalid(
            "admission does not match observed processing and SDK execution",
        ));
    }
    Ok(())
}
