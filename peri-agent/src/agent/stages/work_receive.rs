use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::store::PersistedPayload;

use super::work_boundary::{persisted_projection, WorkBoundary};
use super::work_recovery::{recover_work, RecoveredStage};
use super::{ReceiveOutput, StageContext};
use crate::error::AgentResult;
use crate::session::{MessageSource, QueuedMessage, QueuedPayload};
use peri_acp_types::session_resources::SessionResources;
use std::sync::Arc;

struct InboxBatch {
    inbox: crate::session::MessageQueue,
    messages: Option<Vec<QueuedMessage>>,
}

impl Drop for InboxBatch {
    fn drop(&mut self) {
        if let Some(messages) = self.messages.take() {
            self.inbox.push_batch(messages);
        }
    }
}

pub async fn publish_session_inbox(
    resources: Arc<dyn SessionResources>,
    session_id: &str,
    recipient_lifecycle: u64,
    inbox: &crate::session::MessageQueue,
) -> anyhow::Result<Vec<WorkReceipt>> {
    if session_id.is_empty() || recipient_lifecycle == 0 {
        return Err(anyhow::anyhow!(
            "inbox publication requires frozen recipient identity"
        ));
    }
    let mut batch = InboxBatch {
        inbox: inbox.clone(),
        messages: Some(inbox.drain_batch(64)),
    };
    let messages = batch.messages.as_mut().expect("owned inbox batch");
    for message in messages.iter_mut() {
        if message.delivery_id.is_none() {
            message.delivery_id = Some(match &message.payload {
                QueuedPayload::Message(_) if message.source == MessageSource::UserInput => {
                    MessageId::new()
                }
                QueuedPayload::Message(message) => message.id(),
                QueuedPayload::SystemReminder(_) => MessageId::new(),
            });
        }
    }
    let ledger = super::work_ledger::WorkMutationBarrier::new(resources);
    let mut receipts = Vec::new();
    for message in messages.iter() {
        let delivery_id = message.delivery_id.expect("stable inbox identity");
        let identity = delivery_id.as_uuid().to_string();
        let payload = match &message.payload {
            QueuedPayload::Message(message) => PersistedPayload::Message(message.clone()),
            QueuedPayload::SystemReminder(reminder) => PersistedPayload::SystemReminder {
                id: delivery_id,
                reminder: reminder.clone(),
            },
        };
        let content = WorkPayload::from_payload(&payload)?;
        let snapshot = ledger
            .snapshot(&WorkQuery {
                session_id: session_id.into(),
                limit: 1,
            })
            .await?;
        if let Some(prior) = snapshot.state.deliveries.get(&identity) {
            if prior.recipient_lifecycle != recipient_lifecycle
                || prior.publication.event.content != content
                || prior.publication.policy != message.policy
            {
                return Err(anyhow::anyhow!(
                    "inbox hint conflicts with frozen durable delivery"
                ));
            }
            continue;
        }
        let command = WorkCommand {
            session_id: session_id.into(),
            recipient_lifecycle,
            mutation_id: format!("inbox-publish:{session_id}:{recipient_lifecycle}:{identity}"),
            action: WorkAction::PublishDelivery {
                delivery: PublishDelivery {
                    delivery_id: identity.clone(),
                    event: WorkEvent {
                        producer_namespace: "peri-agent.inbox".into(),
                        event_id: identity,
                        event_kind: "agentMessage".into(),
                        causation_id: None,
                        content,
                    },
                    purpose: if message.source == MessageSource::UserInput {
                        DeliveryPurpose::UserInput
                    } else if message.policy.requirement
                        == peri_acp_types::session::MessageRequirement::Required
                    {
                        DeliveryPurpose::Continuation
                    } else {
                        DeliveryPurpose::Observation
                    },
                    policy: message.policy.clone(),
                },
            },
        };
        receipts.push(ledger.commit(&command).await?);
    }
    batch.messages = None;
    Ok(receipts)
}

pub(crate) async fn receive(ctx: &StageContext) -> AgentResult<Option<ReceiveOutput>> {
    let Some(session) = ctx.work.ensure(ctx).await? else {
        return Ok(None);
    };
    super::execution_control::validate(ctx).await?;
    publish_session_inbox(
        session.ledger.resources(),
        &session.admission.session_id,
        session.admission.lifecycle,
        &ctx.session.queue,
    )
    .await?;
    let result = ctx.work.receive_batch(ctx, &session).await;
    result.map(Some).map_err(Into::into)
}

impl WorkBoundary {
    async fn receive_batch(
        &self,
        ctx: &StageContext,
        session: &super::work_pipeline::WorkSession,
    ) -> anyhow::Result<ReceiveOutput> {
        let mut state = self.state.lock().await;
        let mut snapshot = session.snapshot().await?;
        let work_id = state
            .work_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("SDK work identity is missing"))?;
        if !snapshot.state.works.contains_key(&work_id) {
            let candidate = snapshot
                .candidates
                .iter()
                .find(|candidate| {
                    candidate.work_id == work_id
                        && candidate.work_revision == session.admission.work_revision
                        && candidate.batch_id.is_none()
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("SDK admitted exact batch is no longer claimable")
                })?;
            let command = session.command(WorkAction::ClaimBatch {
                guard: session.guard(&snapshot)?,
                batch_id: work_id.clone(),
                delivery_ids: candidate.delivery_ids.clone(),
            });
            session.ledger.commit(&command).await?;
            snapshot = session.snapshot().await?;
        }
        let recovered = recover_work(&snapshot, &work_id)?;
        let live = match recovered.stage {
            RecoveredStage::ReasonReady | RecoveredStage::ActReady { .. } => true,
            RecoveredStage::Finished => false,
            RecoveredStage::ReasonUncertain {
                request_id,
                request,
            } => {
                return Err(anyhow::anyhow!(
                    "model request {request_id} ({}) requires reconciliation, never regeneration",
                    request.request_digest
                ))
            }
            RecoveredStage::ReconcileInvocations { invocation_ids } => {
                return Err(anyhow::anyhow!(
                    "invocations require owner reconciliation before effects: {invocation_ids:?}"
                ))
            }
            RecoveredStage::Blocked {
                reason,
                recovery_condition,
            } => {
                return Err(anyhow::anyhow!(
                    "work blocked: {reason:?}; recovery: {recovery_condition:?}"
                ))
            }
        };
        tracing::trace!(work_id = %recovered.target.work_id, work_revision = recovered.target.expected_work_revision,
            budget_id = %recovered.budget_id, projection_count = recovered.projection.len(), "durable Receive recovery");
        let batch = snapshot
            .state
            .batches
            .get(&recovered.batch_id)
            .ok_or_else(|| anyhow::anyhow!("claimed exact batch is missing"))?;
        let mut input_message_ids = Vec::new();
        let mut delivered_inputs = Vec::new();
        for delivery_id in &recovered.delivery_ids {
            if !batch.projection_versions.contains_key(delivery_id) {
                continue;
            }
            let delivery = &snapshot.state.deliveries[delivery_id];
            let original = persisted_projection(&delivery.publication.event.content)?;
            if let PersistedPayload::Message(BaseMessage::Human { id, content }) = &original {
                if recovered.processing_delivery_ids.contains(delivery_id) {
                    input_message_ids.push(*id);
                    delivered_inputs.push((*id, content.clone()));
                }
            }
            let payload = persisted_projection(&delivery.projection)?;
            ctx.session
                .transcript
                .write()
                .mirror_committed_payload(payload);
        }
        if let Some(mailbox) = &ctx.session.user_input_mailbox {
            mailbox.mark_claimed(&input_message_ids);
            let delivered = mailbox.mark_delivered(&input_message_ids);
            for (id, content) in delivered_inputs {
                let input_id = id.as_uuid().to_string();
                if delivered.contains(&input_id) {
                    ctx.runtime.event_bus.emit_render(
                        crate::agent::events_v2::RenderEvent::UserInputDelivered {
                            turn_id: ctx.turn_id(),
                            agent_id: ctx.session.agent_id,
                            generation: mailbox.generation().to_owned(),
                            input_id,
                            content,
                        },
                    );
                }
            }
        }
        state.work_id = Some(work_id);
        Ok(ReceiveOutput {
            consumed_count: batch.delivery_ids.len(),
            wake_up_count: usize::from(live),
            input_message_ids,
        })
    }
}
