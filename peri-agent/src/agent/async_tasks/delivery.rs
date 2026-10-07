//! Durable terminal-reminder delivery into the session that initiated a task.
//!
//! A durable Inbox settlement receipt is the delivery acknowledgment.
//! The frozen recipient lifecycle and persisted invocation/task binding are
//! verified before publication. Receive alone projects the canonical reminder;
//! a queue push is only a wake hint, never the owner acknowledgment.

use sha2::{Digest, Sha256};
use std::sync::Arc;

use peri_acp_types::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::tasks::TaskTerminalDelivery;

use crate::session::transcript::MessageTranscript;
use crate::thread::ThreadId;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::store::PersistedPayload;

pub fn durable_task_terminal_delivery(
    resources: Arc<dyn SessionResources>,
    session_id: String,
    recipient_lifecycle: u64,
    queue: MessageQueue,
) -> Arc<dyn TaskTerminalDelivery> {
    SessionTerminalDelivery::for_session(resources, session_id, recipient_lifecycle, queue)
}

pub fn build_task_terminal_command(
    session_id: &str,
    recipient_lifecycle: u64,
    binding: &TaskBinding,
    delivery_id: peri_acp_types::messages::MessageId,
    reminder: &TrustedSystemReminder,
    source: MessageSource,
) -> Result<WorkCommand, String> {
    let task_id = reminder
        .as_reminder()
        .metadata
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("terminal publication has no trusted task identity")?;
    if session_id.is_empty()
        || recipient_lifecycle == 0
        || binding.initiator_session_id != session_id
        || binding.recipient_lifecycle != recipient_lifecycle
        || binding.owner_task_id != task_id
    {
        return Err("terminal publication differs from its immutable task binding".into());
    }
    let identity = delivery_id.as_uuid().to_string();
    let content = WorkPayload::from_payload(&PersistedPayload::SystemReminder {
        id: delivery_id,
        reminder: reminder.clone(),
    })
    .map_err(|error| error.to_string())?;
    let queued = QueuedMessage::system_reminder_with_delivery_id(
        MessageKind::Defer,
        source,
        reminder.clone(),
        delivery_id,
    );
    Ok(WorkCommand {
        session_id: session_id.into(),
        recipient_lifecycle,
        mutation_id: format!("task-terminal:{identity}"),
        action: WorkAction::PublishTaskSettlement {
            delivery: PublishDelivery {
                delivery_id: identity.clone(),
                event: WorkEvent {
                    producer_namespace: "peri-agent.task-terminal".into(),
                    event_id: identity,
                    event_kind: "taskTerminal".into(),
                    causation_id: Some(binding.invocation_id.clone()),
                    content,
                },
                purpose: DeliveryPurpose::TaskTerminal,
                policy: queued.policy,
            },
            binding: binding.clone(),
        },
    })
}

pub(crate) fn terminal_delivery_id(
    task_id: &str,
    terminal_transition_id: &str,
) -> peri_acp_types::messages::MessageId {
    let mut hasher = Sha256::new();
    hasher.update((task_id.len() as u64).to_be_bytes());
    hasher.update(task_id.as_bytes());
    hasher.update((terminal_transition_id.len() as u64).to_be_bytes());
    hasher.update(terminal_transition_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    peri_acp_types::messages::MessageId::from(uuid::Uuid::from_bytes(bytes))
}

/// Delivery route for one initiating session.
pub(crate) struct SessionTerminalDelivery {
    resources: Arc<dyn SessionResources>,
    thread_id: ThreadId,
    recipient_lifecycle: u64,
    queue: MessageQueue,
}

impl SessionTerminalDelivery {
    /// Build a route only when both durable resources and a frozen lifecycle exist.
    pub(crate) fn for_transcript(
        transcript: &Arc<parking_lot::RwLock<MessageTranscript>>,
        queue: &MessageQueue,
        recipient_lifecycle: Option<u64>,
    ) -> Option<Arc<dyn TaskTerminalDelivery>> {
        let (resources, thread_id, _) = transcript.read().idempotent_reminder_port()?;
        let recipient_lifecycle = recipient_lifecycle?;
        Some(Self::for_session(
            resources,
            thread_id,
            recipient_lifecycle,
            queue.clone(),
        ))
    }

    pub(crate) fn for_session(
        resources: Arc<dyn SessionResources>,
        thread_id: ThreadId,
        recipient_lifecycle: u64,
        queue: MessageQueue,
    ) -> Arc<dyn TaskTerminalDelivery> {
        Arc::new(Self {
            resources,
            thread_id,
            recipient_lifecycle,
            queue,
        })
    }
}

impl TaskTerminalDelivery for SessionTerminalDelivery {
    fn deliver<'a>(
        &'a self,
        delivery_id: peri_acp_types::messages::MessageId,
        reminder: &'a TrustedSystemReminder,
        source: MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let snapshot = self
                .resources
                .load_session_work(&WorkQuery {
                    session_id: self.thread_id.clone(),
                    limit: 1,
                })
                .await
                .map_err(|error| error.to_string())?;
            let task_id = reminder
                .as_reminder()
                .metadata
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .ok_or("terminal publication has no trusted task identity")?;
            let binding = snapshot
                .state
                .task_bindings
                .values()
                .find(|binding| {
                    binding.owner_task_id == task_id
                        && binding.initiator_session_id == self.thread_id
                        && binding.recipient_lifecycle == self.recipient_lifecycle
                })
                .ok_or("terminal publication has no durable invocation/task binding")?
                .clone();
            let command = build_task_terminal_command(
                &self.thread_id,
                self.recipient_lifecycle,
                &binding,
                delivery_id,
                reminder,
                source.clone(),
            )?;
            let queued = QueuedMessage::system_reminder_with_delivery_id(
                MessageKind::Defer,
                source,
                reminder.clone(),
                delivery_id,
            );
            let command =
                peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(command)
                    .map_err(|error| error.to_string())?;
            let recipient_matches = snapshot.control.lifecycle == self.recipient_lifecycle;
            drop(snapshot);
            crate::agent::stages::work_ledger::WorkMutationBarrier::new(Arc::clone(
                &self.resources,
            ))
            .commit(&command)
            .await
            .map_err(|error| error.to_string())?;
            if recipient_matches {
                self.queue.push(queued);
            }
            tracing::debug!(
                ?delivery_id,
                "terminal obligation durably accepted by initiator"
            );
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "delivery_test.rs"]
mod tests;
