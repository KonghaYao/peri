use std::sync::Arc;

use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::tasks::TaskTerminalDelivery;
use sha2::{Digest, Sha256};

use crate::session::MessageQueue;

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

pub(crate) struct SessionTerminalDelivery {
    queue: MessageQueue,
}

impl SessionTerminalDelivery {
    pub(crate) fn for_queue(queue: MessageQueue) -> Arc<dyn TaskTerminalDelivery> {
        Arc::new(Self { queue })
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
            self.queue
                .push(QueuedMessage::system_reminder_with_delivery_id(
                    MessageKind::Defer,
                    source,
                    reminder.clone(),
                    delivery_id,
                ));
            Ok(())
        })
    }
}
