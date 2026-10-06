use std::sync::Arc;

use peri_acp_types::session_resources::work::{
    EvidenceQuery, InvocationIntent, InvocationStatus, TaskBinding, WorkAction, WorkCommand,
    WorkDecision, WorkInspection, WorkPage, WorkQuery, WorkReceipt, WorkRejection, WorkResolution,
    WorkSelector, WorkTarget,
};
use peri_acp_types::session_resources::{
    ControlStatus, MutationOutcome, SessionResourceError, SessionResources,
};
use peri_acp_types::tools::ToolContext;
use rmcp::model::RequestMetaObject;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const INVOCATION_META_KEY: &str = "peri.invocation";

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvocationError {
    #[error("MCP invocation requires an exact trusted durable binding")]
    MissingIdentity,
    #[error("MCP invocation does not match its immutable dispatch intent")]
    IdentityConflict,
    #[error("MCP invocation is not durably admitted for dispatch")]
    NotPrepared,
    #[error("MCP invocation outcome is unknown; reconcile the original owner before retrying")]
    OutcomeUnknown,
    #[error("MCP invocation mutation was rejected: {0:?}")]
    Rejected(WorkRejection),
    #[error("MCP invocation mutation ACK is unconfirmed")]
    Unconfirmed { command: Box<WorkCommand> },
    #[error("MCP invocation mutation receipt does not match its command")]
    InvalidReceipt,
    #[error(transparent)]
    Resource(#[from] SessionResourceError),
}

pub(crate) struct McpInvocation {
    resources: Arc<dyn SessionResources>,
    session_id: String,
    lifecycle: u64,
    intent: Arc<InvocationIntent>,
    target: WorkTarget,
    effective_arguments_json: String,
}

impl McpInvocation {
    pub(crate) async fn from_context(
        context: &ToolContext<'_>,
        input: &Value,
        effective_tool: &str,
        owner_identity: &str,
    ) -> Result<Self, InvocationError> {
        let session_id = context
            .session_id
            .as_ref()
            .filter(|identity| !identity.is_empty())
            .ok_or(InvocationError::MissingIdentity)?;
        let lifecycle = context
            .session_lifecycle
            .filter(|lifecycle| *lifecycle > 0)
            .ok_or(InvocationError::MissingIdentity)?;
        let intent = context
            .invocation_intent
            .clone()
            .ok_or(InvocationError::MissingIdentity)?;
        let target = context
            .invocation_work_target
            .clone()
            .ok_or(InvocationError::MissingIdentity)?;
        let resources = context
            .session_resources
            .clone()
            .ok_or(InvocationError::MissingIdentity)?;
        let evidence = resources
            .read_evidence(&EvidenceQuery {
                session_id: session_id.clone(),
                reference: intent.effective_arguments.clone(),
            })
            .await?;
        evidence.validate()?;
        if evidence.reference != intent.effective_arguments {
            return Err(InvocationError::IdentityConflict);
        }
        let effective_arguments_json =
            String::from_utf8(evidence.bytes).map_err(|_| InvocationError::IdentityConflict)?;
        let parsed: Value = serde_json::from_str(&effective_arguments_json)
            .map_err(|_| InvocationError::IdentityConflict)?;
        if context.invocation_id.as_deref() != Some(intent.invocation_id.as_str())
            || intent.effective_tool_name != effective_tool
            || intent.owner_identity != owner_identity
            || intent.scope_id != *session_id
            || intent.authorization_ref.is_empty()
            || intent.recovery_locator.is_empty()
            || intent.effective_arguments_digest
                != format!("{:x}", Sha256::digest(effective_arguments_json.as_bytes()))
            || parsed != *input
            || target.work_id.is_empty()
        {
            return Err(InvocationError::IdentityConflict);
        }
        Ok(Self {
            resources,
            session_id: session_id.clone(),
            lifecycle,
            intent,
            target,
            effective_arguments_json,
        })
    }

    pub(crate) async fn prepare(&self) -> Result<(), InvocationError> {
        let snapshot = self.snapshot().await?;
        self.validate_record(&snapshot)?;
        if snapshot.control.lifecycle != self.lifecycle
            || snapshot.control.status != ControlStatus::Active
        {
            return Err(InvocationError::NotPrepared);
        }
        Ok(())
    }

    pub(crate) fn request_meta(
        &self,
        existing: Option<RequestMetaObject>,
    ) -> Result<RequestMetaObject, InvocationError> {
        let mut metadata = existing.unwrap_or_default();
        if metadata.0 .0.contains_key(INVOCATION_META_KEY) {
            return Err(InvocationError::IdentityConflict);
        }
        metadata.0 .0.insert(
            INVOCATION_META_KEY.into(),
            serde_json::json!({
                "version": 1,
                "invocationId": self.intent.invocation_id,
                "initiatorSessionId": self.session_id,
                "recipientLifecycle": self.lifecycle,
                "argumentsDigest": self.intent.effective_arguments_digest,
                "argumentsJson": self.effective_arguments_json,
                "toolName": self.intent.effective_tool_name,
                "ownerIdentity": self.intent.owner_identity,
                "scopeId": self.intent.scope_id,
                "scopeEpoch": self.intent.scope_epoch,
                "authorizationRef": self.intent.authorization_ref,
            }),
        );
        Ok(metadata)
    }

    pub(crate) async fn bind_task(
        &self,
        owner_task_id: &str,
    ) -> Result<TaskBinding, InvocationError> {
        if owner_task_id.is_empty() {
            return Err(InvocationError::IdentityConflict);
        }
        let binding = TaskBinding {
            invocation_id: self.intent.invocation_id.clone(),
            owner_identity: self.intent.owner_identity.clone(),
            owner_task_id: owner_task_id.into(),
            initiator_session_id: self.session_id.clone(),
            recipient_lifecycle: self.lifecycle,
            recovery_locator: self.intent.recovery_locator.clone(),
            authorization_ref: self.intent.authorization_ref.clone(),
        };
        for _ in 0..3 {
            let snapshot = self.snapshot().await?;
            if let Some(prior) = effect(&snapshot)?.binding.as_ref() {
                return if prior == &binding {
                    Ok(binding)
                } else {
                    Err(InvocationError::IdentityConflict)
                };
            }
            self.validate_record(&snapshot)?;
            let command = self.command(WorkAction::ReconcileTaskBinding {
                expected_revision: snapshot.head.change_seq,
                binding: binding.clone(),
            })?;
            match commit(self.resources.as_ref(), &command).await {
                Ok(_) => return Ok(binding),
                Err(InvocationError::Rejected(WorkRejection::StaleRevision)) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(InvocationError::Rejected(WorkRejection::StaleRevision))
    }

    pub(crate) async fn record_outcome_unknown(&self) -> Result<(), InvocationError> {
        for _ in 0..3 {
            let snapshot = self.snapshot().await?;
            let record = effect(&snapshot)?;
            if record.intent != *self.intent || record.recipient_lifecycle != self.lifecycle {
                return Err(InvocationError::IdentityConflict);
            }
            if record.status == InvocationStatus::OutcomeUnknown {
                return Ok(());
            }
            let processing = self
                .resources
                .inspect_work(&WorkQuery::new(
                    &self.session_id,
                    WorkSelector::Processing {
                        processing_id: self.target.work_id.clone(),
                    },
                ))
                .await?;
            let WorkPage::Processings(records) = &processing.page else {
                return Err(InvocationError::IdentityConflict);
            };
            let work = records.first().ok_or(InvocationError::NotPrepared)?;
            let command = self.command(WorkAction::OutcomeUnknown {
                expected_revision: processing.head.change_seq,
                target: WorkTarget {
                    work_id: self.target.work_id.clone(),
                    expected_work_revision: work.revision,
                },
                invocation_id: self.intent.invocation_id.clone(),
                expected_effect_revision: record.revision,
                reason: "MCP response or durable task ACK unavailable; original owner outcome unconfirmed".into(),
            })?;
            match commit(self.resources.as_ref(), &command).await {
                Err(InvocationError::Rejected(WorkRejection::StaleRevision)) => continue,
                result => return result.map(|_| ()),
            }
        }
        Err(InvocationError::Rejected(WorkRejection::StaleRevision))
    }

    async fn snapshot(&self) -> Result<WorkInspection, InvocationError> {
        let snapshot = self
            .resources
            .inspect_work(&WorkQuery {
                session_id: self.session_id.clone(),
                selector: WorkSelector::Effect {
                    invocation_id: self.intent.invocation_id.clone(),
                },
                limit: 1,
                cursor: None,
            })
            .await?;
        if snapshot.session_id != self.session_id {
            return Err(InvocationError::IdentityConflict);
        }
        Ok(snapshot)
    }

    fn validate_record(&self, snapshot: &WorkInspection) -> Result<(), InvocationError> {
        let record = effect(snapshot)?;
        if record.invocation_id != self.intent.invocation_id
            || record.intent != *self.intent
            || record.recipient_lifecycle != self.lifecycle
            || record.processing_id.as_deref() != Some(self.target.work_id.as_str())
        {
            return Err(InvocationError::IdentityConflict);
        }
        match record.status {
            InvocationStatus::DispatchAccepted => Ok(()),
            InvocationStatus::OutcomeUnknown => Err(InvocationError::OutcomeUnknown),
            _ => Err(InvocationError::NotPrepared),
        }
    }

    fn command(&self, action: WorkAction) -> Result<WorkCommand, InvocationError> {
        let mut command = WorkCommand {
            session_id: self.session_id.clone(),
            recipient_lifecycle: self.lifecycle,
            mutation_id: "mcp-invocation".into(),
            action,
        };
        command.mutation_id = format!("mcp-invocation:{}", command.digest()?);
        Ok(command)
    }
}

fn effect(
    snapshot: &WorkInspection,
) -> Result<&peri_acp_types::session_resources::work::Effect, InvocationError> {
    let WorkPage::Effects(records) = &snapshot.page else {
        return Err(InvocationError::IdentityConflict);
    };
    if records.len() != 1 {
        return Err(InvocationError::NotPrepared);
    }
    Ok(&records[0])
}

pub(crate) async fn commit(
    resources: &dyn SessionResources,
    command: &WorkCommand,
) -> Result<WorkReceipt, InvocationError> {
    let receipt = match resources.apply_work_mutation(command).await {
        Ok(receipt) => receipt,
        Err(error) if error.effect() == MutationOutcome::Unknown => {
            match resources.resolve_work_mutation(command).await {
                Ok(WorkResolution::Applied { receipt }) => receipt,
                _ => {
                    return Err(InvocationError::Unconfirmed {
                        command: Box::new(command.clone()),
                    });
                }
            }
        }
        Err(error) => return Err(error.into()),
    };
    if receipt.session_id != command.session_id || receipt.mutation_id != command.mutation_id {
        return Err(InvocationError::InvalidReceipt);
    }
    match &receipt.decision {
        WorkDecision::Accepted => Ok(receipt),
        WorkDecision::Rejected { reason } => Err(InvocationError::Rejected(reason.clone())),
    }
}

#[cfg(test)]
#[path = "invocation_test.rs"]
mod tests;
