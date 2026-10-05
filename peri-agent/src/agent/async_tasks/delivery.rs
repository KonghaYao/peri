//! Durable terminal-reminder delivery into the session that initiated a task.
//!
//! Canonical commit is the delivery: the reminder lands in the initiator's
//! transcript under a stable delivery ID, so an inactive or already-finished
//! initiator still sees it after resume, and a crash before the commit can
//! retry. The queue push is only a wake-up for a live initiator; it is not
//! treated as delivery.

use sha2::{Digest, Sha256};
use std::sync::Arc;

use peri_acp_types::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::tasks::TaskTerminalDelivery;

use crate::session::transcript::{MessageTranscript, PersistOp};
use crate::thread::ThreadId;

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
    writer: Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>>,
    queue: MessageQueue,
}

impl SessionTerminalDelivery {
    /// Build the route when the session's transcript is persisted; sessions
    /// without a store keep the previous queue-only behavior.
    pub(crate) fn for_transcript(
        transcript: &Arc<parking_lot::RwLock<MessageTranscript>>,
        queue: &MessageQueue,
    ) -> Option<Arc<dyn TaskTerminalDelivery>> {
        let (resources, thread_id, writer) = transcript.read().idempotent_reminder_port()?;
        Some(Arc::new(Self {
            resources,
            thread_id,
            writer,
            queue: queue.clone(),
        }))
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
            // The session's own writer is FIFO; flushing first keeps the
            // reminder after everything the session already committed.
            if let Some(writer) = &self.writer {
                MessageTranscript::flush_via_tx(writer)
                    .await
                    .map_err(|error| format!("terminal reminder flush failed: {error}"))?;
            }
            self.resources
                .append_reminder_if_absent(&self.thread_id, delivery_id, reminder)
                .await
                .map_err(|error| format!("terminal reminder commit failed: {error}"))?;
            let queued = QueuedMessage::system_reminder_with_delivery_id(
                MessageKind::Defer,
                source,
                reminder.clone(),
                delivery_id,
            );
            self.queue.push(queued);
            tracing::debug!(?delivery_id, "terminal reminder committed to initiator");
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "delivery_test.rs"]
mod tests;
