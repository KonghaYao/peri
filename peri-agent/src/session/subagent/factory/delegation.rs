use std::sync::Arc;

use peri_acp_types::session::{MessageQueue, QueuedMessage, QueuedPayload};
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, PublishDelivery, TaskBinding, WorkAction, WorkCommand, WorkEvent, WorkPage,
    WorkQuery, WorkSelector,
};
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;

use crate::agent::stages::work_ledger::WorkMutationBarrier;
use crate::tools::{EffectiveToolError, EffectiveToolErrorCode};

pub(super) enum DelegationInputMode {
    FollowUp,
    ReplaceProcessing,
}

pub(super) async fn parent_tool_call_id(
    resources: Option<&dyn SessionResources>,
    initiator: Option<&str>,
    invocation_id: Option<&str>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(invocation_id) = invocation_id else {
        return Ok(None);
    };
    let resources = resources.ok_or_else(|| {
        tracing::error!(
            invocation_id,
            "delegation resources unavailable for event identity"
        );
        "Blocked: delegation resources unavailable"
    })?;
    let initiator = initiator.ok_or_else(|| {
        tracing::error!(
            invocation_id,
            "delegation initiator unavailable for event identity"
        );
        "Blocked: delegation initiator unavailable"
    })?;
    let invocation =
        crate::session::work_access::effect(resources, initiator, invocation_id).await?;
    if invocation.intent.tool_call_id.is_empty() {
        tracing::error!(
            invocation_id,
            "delegation invocation has no tool-call identity"
        );
        return Err("Incomplete: delegation tool-call identity unavailable".into());
    }
    Ok(Some(invocation.intent.tool_call_id.clone()))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::session::subagent) async fn publish_work_delegation(
    resources: Arc<dyn SessionResources>,
    child_id: &str,
    lifecycle: u64,
    queue: &MessageQueue,
    mut message: QueuedMessage,
    initiator: &str,
    invocation_id: &str,
    task_id: &str,
    input_mode: DelegationInputMode,
) -> Result<TaskBinding, Box<dyn std::error::Error + Send + Sync>> {
    let barrier = WorkMutationBarrier::new(resources.clone());
    let child = barrier
        .inspect(&WorkQuery::new(child_id, WorkSelector::Head))
        .await?;
    let unresolved = child.head.current_processing_id.is_some();
    if child.control.lifecycle != lifecycle
        || child.control.status != peri_acp_types::session_resources::ControlStatus::Active
        || child.control.attempt.is_some()
        || child.head.terminal_obligations != 0
        || child.head.unresolved_effects != 0
        || child.head.legacy_unknown != 0
        || (matches!(input_mode, DelegationInputMode::FollowUp) && unresolved)
    {
        tracing::warn!(
            child_thread_id = child_id,
            "delegation input rejected before publication: child requires reconciliation"
        );
        return Err(Box::new(EffectiveToolError::new(
            EffectiveToolErrorCode::ApplicationFailed,
            "Blocked: child requires reconciliation before delegation; provide explicit input only when authorized to replace prior processing",
        )));
    }
    let invocation =
        crate::session::work_access::effect(resources.as_ref(), initiator, invocation_id).await?;
    let parent_binding_receipt =
        super::cold::bind_delegation_task(resources.as_ref(), initiator, &invocation, task_id)
            .await?;
    let binding = TaskBinding {
        invocation_id: invocation_id.into(),
        owner_identity: invocation.intent.owner_identity.clone(),
        owner_task_id: task_id.into(),
        initiator_session_id: initiator.into(),
        recipient_lifecycle: invocation.recipient_lifecycle,
        recovery_locator: invocation.intent.recovery_locator.clone(),
        authorization_ref: invocation.intent.authorization_ref.clone(),
    };
    let delivery_id = message.delivery_id.unwrap_or_default();
    message.delivery_id = Some(delivery_id);
    let payload = match &message.payload {
        QueuedPayload::Message(message) => PersistedPayload::Message(message.clone()),
        QueuedPayload::SystemReminder(reminder) => PersistedPayload::SystemReminder {
            id: delivery_id,
            reminder: reminder.clone(),
        },
    };
    let identity = delivery_id.as_uuid().to_string();
    let mut delivery = PublishDelivery {
        delivery_id: identity.clone(),
        event: WorkEvent {
            producer_namespace: "peri-agent.child-delegation".into(),
            event_id: identity.clone(),
            event_kind: "delegatedInput".into(),
            causation_id: Some(invocation_id.into()),
            content: crate::agent::stages::prepare_work_payload(
                resources.as_ref(),
                child_id,
                &payload,
            )
            .await?,
        },
        purpose: DeliveryPurpose::UserInput,
        policy: message.policy.clone(),
    };
    let action = match input_mode {
        DelegationInputMode::FollowUp => WorkAction::PublishDelivery { delivery },
        DelegationInputMode::ReplaceProcessing => {
            use sha2::{Digest, Sha256};
            let PersistedPayload::Message(crate::messages::BaseMessage::Human { id, content }) =
                &payload
            else {
                return Err(Box::new(EffectiveToolError::new(
                    EffectiveToolErrorCode::InvalidInput,
                    "delegation input must be a human prompt",
                )));
            };
            let input = peri_acp_types::session::UserInput {
                input_id: id.as_uuid().to_string(),
                content: content.clone(),
                original_draft: content.text_content(),
            };
            let input_json = serde_json::to_string(&input)?;
            let digest = Sha256::digest(input_json.as_bytes());
            let mut fingerprint_bytes = [0_u8; 8];
            fingerprint_bytes.copy_from_slice(&digest[..8]);
            let stage = WorkCommand {
                session_id: child_id.into(),
                recipient_lifecycle: lifecycle,
                mutation_id: format!("child-delegation-stage:{identity}"),
                action: WorkAction::StageUserInput {
                    input_id: input.input_id.clone(),
                    content: crate::agent::stages::prepare_work_evidence(
                        resources.as_ref(),
                        child_id,
                        input_json.into_bytes(),
                    )
                    .await?,
                    command_id: invocation_id.into(),
                    fingerprint: u64::from_be_bytes(fingerprint_bytes),
                },
            };
            barrier.commit(&stage).await?;
            let staged = barrier
                .inspect(&WorkQuery::new(
                    child_id,
                    WorkSelector::Draft {
                        input_id: input.input_id.clone(),
                    },
                ))
                .await?;
            let WorkPage::Drafts(drafts) = staged.page else {
                return Err("delegation draft lookup returned a different page".into());
            };
            let draft = drafts
                .first()
                .filter(|draft| {
                    drafts.len() == 1
                        && draft.input_id == input.input_id
                        && draft.command_id == invocation_id
                        && draft.recipient_lifecycle == lifecycle
                })
                .ok_or("exact delegation draft unavailable")?;
            delivery.event.causation_id = Some(serde_json::to_string(
                &peri_acp_types::session_resources::work::UserInputPublicationIdentity {
                    input_id: input.input_id,
                    publication_generation: invocation_id.into(),
                    fingerprint: draft.fingerprint,
                    command_id: invocation_id.into(),
                    draft_binding:
                        peri_acp_types::session_resources::work::StagedUserInputPublicationBinding {
                            draft_revision: draft.revision,
                            draft_fingerprint: draft.fingerprint,
                            canonical_content: delivery.event.content.content.clone(),
                        },
                },
            )?);
            let current = barrier
                .inspect(&WorkQuery::new(child_id, WorkSelector::Head))
                .await?;
            WorkAction::PublishStagedUserInputs {
                expected_revision: current.head.change_seq,
                expected_control_generation: current.control.control_generation,
                expected_attempt: current.control.attempt,
                interrupt_current: true,
                deliveries: vec![delivery],
            }
        }
    };
    let command = WorkCommand {
        session_id: child_id.into(),
        recipient_lifecycle: lifecycle,
        mutation_id: format!("child-delegation-input:{identity}"),
        action,
    };
    barrier.commit(&command).await?;
    let child = resources
        .inspect_work(&WorkQuery::new(child_id, WorkSelector::Availability))
        .await?;
    let WorkPage::Availability(availability) = &child.page else {
        return Err("Blocked: delegation availability returned a different page".into());
    };
    let candidate = availability
        .candidates
        .iter()
        .find(|candidate| candidate.delivery_ids.contains(&identity))
        .ok_or("Blocked: current delegation work requires reconciliation before execution")?;
    let mut command = WorkCommand {
        session_id: child_id.into(),
        recipient_lifecycle: lifecycle,
        mutation_id: "child-work-delegation".into(),
        action: WorkAction::BindWorkDelegation {
            expected_revision: child.head.change_seq,
            work_id: candidate.work_id.clone(),
            binding: binding.clone(),
            parent_binding_receipt,
        },
    };
    command.mutation_id = format!("child-work-delegation:{}", command.digest()?);
    barrier.commit(&command).await?;
    queue.push(message);
    Ok(binding)
}
