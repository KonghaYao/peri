//! Background sub-agent message admission. The existing task entry owns the route.

use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use tokio_util::sync::CancellationToken;

use crate::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};

const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_PENDING_MESSAGES: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum SubagentMessageError {
    #[error("send_subagent: active sub-agent requires a non-empty prompt")]
    EmptyPrompt,
    #[error("send_subagent: prompt exceeds {MAX_MESSAGE_BYTES} bytes")]
    TooLarge,
    #[error("send_subagent: background sub-agent is closing; message was not queued")]
    Closed,
    #[error("send_subagent: inbox is full; message was not queued")]
    Full,
    #[error("send_subagent: invalid message envelope: {0}")]
    InvalidEnvelope(#[from] peri_acp_types::system_reminder::ReminderValidationError),
    #[error("send_subagent: MQ admission failed: {0}")]
    Store(String),
}

/// A queue receipt, not a claim that the model has read or persisted the message.
#[derive(Debug)]
pub struct QueuedSubagentMessage {
    pub task_id: String,
}

/// Revocable routing capability attached only to background Agent tasks.
pub struct BackgroundAgentInbox {
    pub(super) thread_id: String,
    queue: Mutex<Option<MessageQueue>>,
    cancel: CancellationToken,
}

impl BackgroundAgentInbox {
    /// 仅测试：同步适配同一 async 入队实现。
    #[cfg(test)]
    pub(crate) fn send_sync(
        &self,
        task_id: &str,
        prompt: Option<&str>,
    ) -> Result<QueuedSubagentMessage, SubagentMessageError> {
        futures::executor::block_on(self.send(task_id, prompt))
    }

    pub(crate) fn new(
        thread_id: String,
        queue: MessageQueue,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            thread_id,
            queue: Mutex::new(Some(queue)),
            cancel,
        })
    }

    pub(super) async fn send(
        &self,
        task_id: &str,
        prompt: Option<&str>,
    ) -> Result<QueuedSubagentMessage, SubagentMessageError> {
        let queue = {
            let guard = self.queue.lock();
            let queue = guard.as_ref().ok_or(SubagentMessageError::Closed)?;
            queue.clone()
        };
        if self.cancel.is_cancelled() {
            return Err(SubagentMessageError::Closed);
        }
        let prompt = prompt
            .filter(|text| !text.trim().is_empty())
            .ok_or(SubagentMessageError::EmptyPrompt)?;
        if prompt.len() > MAX_MESSAGE_BYTES {
            return Err(SubagentMessageError::TooLarge);
        }
        // Only this producer's admission is bounded; other queue producers retain
        // their existing semantics. The admission lock serializes concurrent sends.
        if queue
            .len()
            .await
            .map_err(|error| SubagentMessageError::Store(error.to_string()))?
            >= MAX_PENDING_MESSAGES
        {
            return Err(SubagentMessageError::Full);
        }
        let reminder = TrustedSystemReminderFactory::for_producer().construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("subagent".into()),
            kind: "parent_message".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
            body: format!("Supplemental message from the parent agent:\n{prompt}"),
            summary: Some("Parent agent message".into()),
            metadata: serde_json::json!({
                "child_thread_id": self.thread_id,
                "task_id": task_id,
            }),
        })?;
        queue
            .enqueue(QueuedMessage::system_reminder(
                MessageKind::Defer,
                MessageSource::SystemInjected,
                reminder,
            ))
            .await
            .map_err(|error| SubagentMessageError::Store(error.to_string()))?;
        Ok(QueuedSubagentMessage {
            task_id: task_id.to_owned(),
        })
    }

    pub(crate) fn close(&self) {
        self.queue.lock().take();
    }
}

/// Captured before spawning so an unpolled, aborted, or panicking execution also
/// revokes its inbox. Normal execution closes it as soon as the loop returns.
pub(crate) struct BackgroundAgentInboxGuard(pub(crate) Arc<BackgroundAgentInbox>);

impl Drop for BackgroundAgentInboxGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
#[path = "agent_inbox_test.rs"]
mod tests;
