use std::sync::Arc;

use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{MessageQueue, QueuedMessage, QueuedPayload};
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, PublishDelivery, TaskBinding, WorkAction, WorkCommand, WorkEvent, WorkPayload,
    WorkQuery,
};
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;

use crate::agent::stages::work_ledger::WorkMutationBarrier;

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
) -> Result<TaskBinding, Box<dyn std::error::Error + Send + Sync>> {
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
    let delivery_id = message.delivery_id.unwrap_or_else(MessageId::new);
    message.delivery_id = Some(delivery_id);
    let payload = match &message.payload {
        QueuedPayload::Message(message) => PersistedPayload::Message(message.clone()),
        QueuedPayload::SystemReminder(reminder) => PersistedPayload::SystemReminder {
            id: delivery_id,
            reminder: reminder.clone(),
        },
    };
    let identity = delivery_id.as_uuid().to_string();
    let barrier = WorkMutationBarrier::new(resources.clone());
    let command = WorkCommand {
        session_id: child_id.into(),
        recipient_lifecycle: lifecycle,
        mutation_id: format!("child-delegation-input:{identity}"),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
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
            },
        },
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
