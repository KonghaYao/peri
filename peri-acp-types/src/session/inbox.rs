//! Session inbox 与生产者 handle 是同一 mailbox 的消费与发布能力。

use super::{MessageKind, MessageQueue, MessageSource, MqEnqueueOutcome, QueuedMessage};
use crate::messages::BaseMessage;
use crate::system_reminder::TrustedSystemReminder;
use anyhow::Result;
use std::sync::Arc;

// ─── SessionInbox / InboxHandle ──────────────────────────────────────────────

/// Wraps the persistent MQ facade with an async await-wake mechanism.
///
/// During the ReAct loop, the Receive stage selects and hands off messages through
/// [`MessageQueue`] — no wake needed (the loop is already spinning).
///
/// During IDLE (between ReAct loops), the ACP executor calls
/// [`await_wake`](Self::await_wake), which blocks until MQ holds new work, then the
/// loop resumes and re-reads the facts.
pub struct SessionInbox {
    queue: Arc<MessageQueue>,
}

impl SessionInbox {
    /// Create a new SessionInbox wrapping the given queue.
    ///
    /// The queue is typically the session-level shared instance passed through
    /// `Session::new_with_cancel_and_queue`.
    pub fn new(queue: Arc<MessageQueue>) -> Self {
        Self { queue }
    }

    /// Block until MQ holds at least one wake-able message (see [`MessageQueue::has_wake_up`]).
    ///
    /// Called by ACP executor's `run_session_loop` when the previous iteration ends
    /// with `should_continue = false` (no more messages to process).
    ///
    /// ## Non-destructive
    ///
    /// This method does NOT consume any messages. The actual consumption happens in
    /// the Receive stage via [`MessageQueue::select_for_receive`]/[`MessageQueue::receive`].
    ///
    /// ## Spurious wakeup guard
    ///
    /// After waking, the MQ facts are re-checked. If only non-waking messages arrived,
    /// we go back to waiting. This prevents the executor from spinning on Info-only
    /// notifications.
    pub async fn await_wake(&self) {
        self.queue.await_wake().await;
    }

    /// Get a cloneable handle for producers.
    ///
    /// Producers (cron owner, channel owner, async router for bg_results, etc.)
    /// use this handle to push messages and wake the idle executor.
    pub fn handle(&self) -> InboxHandle {
        InboxHandle {
            queue: Arc::clone(&self.queue),
        }
    }

    /// Access the underlying MessageQueue facade.
    ///
    /// The facade exposes no destructive memory operations: reading questions are
    /// answered by the store, and consumption only happens through Receive.
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }
}

impl std::fmt::Debug for SessionInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInbox")
            .field("queue", &self.queue)
            .finish()
    }
}

/// Cloneable handle for pushing messages into the SessionInbox.
///
/// Producers (cron_owner, async_router for bg_results) hold this
/// handle to push messages and wake the idle executor. The handle is `Send + Sync`
/// and cheaply cloneable — safe to store in long-lived components.
///
/// TUI should NOT have access to this handle.
#[derive(Clone)]
pub struct InboxHandle {
    queue: Arc<MessageQueue>,
}

impl InboxHandle {
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }

    /// Push a Prompt message (user input or external request) and wake the executor.
    ///
    /// Prompt messages are consumed by the Receive stage and wake the loop.
    pub async fn push_prompt(
        &self,
        source: MessageSource,
        message: BaseMessage,
    ) -> Result<MqEnqueueOutcome> {
        let queued = QueuedMessage::prompt(source, message);
        if queued.source == MessageSource::UserInput {
            // 直接 prompt 没有待发送区：入队即放行，否则 Receive 会按未 dispatch 跳过。
            self.queue.enqueue_sent(queued).await
        } else {
            self.queue.enqueue(queued).await
        }
    }

    /// Push a Defer message (SubAgent complete, Cron trigger, bg result) and wake.
    pub async fn push_defer(
        &self,
        source: MessageSource,
        message: BaseMessage,
    ) -> Result<MqEnqueueOutcome> {
        self.queue
            .enqueue(QueuedMessage::defer(source, message))
            .await
    }

    /// Push an Info message (system reminder, hook injection) — does NOT wake.
    ///
    /// Info messages are handed off by Receive, but never wake the loop.
    /// They must be carried out by a Prompt message arriving later.
    pub async fn push_info(
        &self,
        source: MessageSource,
        message: BaseMessage,
    ) -> Result<MqEnqueueOutcome> {
        self.queue
            .enqueue(QueuedMessage::info(source, message))
            .await
    }

    /// Push a trusted reminder and preserve its scheduling semantics.
    pub async fn push_system_reminder(
        &self,
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
    ) -> Result<MqEnqueueOutcome> {
        self.push(QueuedMessage::system_reminder(kind, source, reminder))
            .await
    }

    pub async fn push_system_reminder_with_delivery_id(
        &self,
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
        delivery_id: crate::messages::MessageId,
    ) -> Result<MqEnqueueOutcome> {
        self.push(QueuedMessage::system_reminder_with_delivery_id(
            kind,
            source,
            reminder,
            delivery_id,
        ))
        .await
    }

    /// Push an arbitrary QueuedMessage and conditionally wake.
    ///
    /// Wakes only for a non-passive policy (i.e., `policy.notifies_execution()`).
    pub async fn push(&self, msg: QueuedMessage) -> Result<MqEnqueueOutcome> {
        self.queue.enqueue(msg).await
    }

    /// Batch push messages; commits one by one and wakes once if any was wake-able.
    pub async fn push_batch(&self, msgs: Vec<QueuedMessage>) -> Result<Vec<MqEnqueueOutcome>> {
        self.queue.enqueue_batch(msgs).await
    }
}

impl std::fmt::Debug for InboxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxHandle")
            .field("queue", &self.queue)
            .finish()
    }
}
