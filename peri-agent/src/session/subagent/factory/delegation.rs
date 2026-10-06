use std::sync::Arc;

use peri_acp_types::session::{MessageQueue, QueuedMessage, QueuedPayload};
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, PublishDelivery, TaskBinding, WorkAction, WorkCommand, WorkEvent, WorkPayload,
    WorkQuery,
};
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;

use crate::agent::stages::work_ledger::WorkMutationBarrier;
use crate::tools::{EffectiveToolError, EffectiveToolErrorCode};

pub(super) enum DelegationInputMode {
    FollowUp,
    ReplaceProcessing,
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
        .snapshot(&WorkQuery {
            session_id: child_id.into(),
            limit: 64,
        })
        .await?;
    let unresolved = child.state.works.values().any(|work| {
        child.state.work_lifecycle(&work.work_id) == Some(lifecycle)
            && !matches!(
                work.stage,
                peri_acp_types::session_resources::work::WorkStage::Settled
                    | peri_acp_types::session_resources::work::WorkStage::Abandoned
            )
    });
    if child.control.lifecycle != lifecycle
        || child.control.status != peri_acp_types::session_resources::ControlStatus::Active
        || child.control.attempt.is_some()
        || child.state.has_pending_terminal_obligations_for(lifecycle)
        || child.state.has_unknown_live_work_lifecycle()
        || !child.state.legacy_unknown.is_empty()
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
    let parent = resources
        .load_session_work(&WorkQuery {
            session_id: initiator.into(),
            limit: 1,
        })
        .await?;
    let invocation = parent
        .state
        .invocations
        .get(invocation_id)
        .ok_or("Blocked: current trusted delegation invocation unavailable")?;
    let parent_binding_receipt =
        super::cold::bind_delegation_task(resources.as_ref(), initiator, invocation, task_id)
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
    let delivery = PublishDelivery {
        delivery_id: identity.clone(),
        event: WorkEvent {
            producer_namespace: "peri-agent.child-delegation".into(),
            event_id: identity.clone(),
            event_kind: "delegatedInput".into(),
            causation_id: Some(invocation_id.into()),
            content: WorkPayload::from_payload(&payload)?,
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
                    input_json,
                    command_id: invocation_id.into(),
                    fingerprint: u64::from_be_bytes(fingerprint_bytes),
                },
            };
            barrier.commit(&stage).await?;
            let current = barrier
                .snapshot(&WorkQuery {
                    session_id: child_id.into(),
                    limit: 64,
                })
                .await?;
            WorkAction::PublishStagedUserInputs {
                expected_revision: current.state.revision,
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
        .load_session_work(&WorkQuery {
            session_id: child_id.into(),
            limit: 64,
        })
        .await?;
    let candidate = child
        .candidates
        .iter()
        .find(|candidate| candidate.delivery_ids.contains(&identity))
        .ok_or("Blocked: current delegation work requires reconciliation before execution")?;
    let mut command = WorkCommand {
        session_id: child_id.into(),
        recipient_lifecycle: lifecycle,
        mutation_id: "child-work-delegation".into(),
        action: WorkAction::BindWorkDelegation {
            expected_revision: child.state.revision,
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
