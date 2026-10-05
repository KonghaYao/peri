//! Session inbox 与生产者 handle 是同一 mailbox 的消费与发布能力。

use super::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
use crate::{messages::BaseMessage, system_reminder::TrustedSystemReminder};
use std::sync::Arc;

// ─── SessionInbox / InboxHandle ──────────────────────────────────────────────

/// Wraps the existing v2 MessageQueue with an async await-wake mechanism.
///
/// During ReAct loop, `stages/receive.rs` calls `drain_all`
/// to consume pending messages — no wake needed (loop is already spinning).
///
/// During IDLE (between ReAct loops), the ACP executor calls [`await_wake`](Self::await_wake)
/// which blocks until a new Prompt/Defer is enqueued, then the loop resumes.
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

    /// Block until the inbox has at least one wake-able message (Prompt or Defer).
    ///
    /// Called by ACP executor's `run_session_loop` when the previous iteration ends
    /// with `should_continue = false` (no more messages to process).
    ///
    /// ## Non-destructive
    ///
    /// This method does NOT drain any messages. The actual consumption happens in
    /// `stages/receive.rs` via `drain_all`.
    ///
    /// ## Spurious wakeup guard
    ///
    /// After waking, we re-check `has_wake_up()`. If only Info messages arrived
    /// (which don't wake the loop), we go back to waiting. This prevents the executor
    /// from spinning on Info-only notifications.
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

    /// Access the underlying MessageQueue (read-only reference).
    ///
    /// Used by stages that need to drain (e.g., `StageContext` construction).
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }
}

impl std::fmt::Debug for SessionInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInbox")
            .field("queue_len", &self.queue.len())
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
    /// Push a Prompt message (user input or external request) and wake the executor.
    ///
    /// Prompt messages are consumed by `drain_all` during the Receive stage
    /// and wake the loop.
    pub fn push_prompt(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::prompt(source, message));
    }

    /// Push a Defer message (SubAgent complete, Cron trigger, bg result) and wake.
    ///
    /// In RCRA, Defer messages are consumed by `drain_all` during the Receive stage.
    pub fn push_defer(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::defer(source, message));
    }

    /// Push an Info message (system reminder, hook injection) — does NOT wake.
    ///
    /// Info messages are consumed by `drain_all`, but never wake the loop.
    /// They must be carried out by a Prompt message arriving later.
    pub fn push_info(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::info(source, message));
    }

    /// Push a trusted reminder and preserve its scheduling semantics.
    pub fn push_system_reminder(
        &self,
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
    ) {
        self.push(QueuedMessage::system_reminder(kind, source, reminder));
    }

    pub fn push_system_reminder_with_delivery_id(
        &self,
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
        delivery_id: crate::messages::MessageId,
    ) {
        self.push(QueuedMessage::system_reminder_with_delivery_id(
            kind,
            source,
            reminder,
            delivery_id,
        ));
    }

    /// Push an arbitrary QueuedMessage and conditionally wake.
    ///
    /// Wakes only if the message kind is Prompt or Defer (i.e., `kind.wakes_up()`).
    pub fn push(&self, msg: QueuedMessage) {
        self.queue.push(msg);
    }

    /// Batch push messages; wakes once if any message is wake-able.
    pub fn push_batch(&self, msgs: Vec<QueuedMessage>) {
        self.queue.push_batch(msgs);
    }
}

impl std::fmt::Debug for InboxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxHandle")
            .field("queue_len", &self.queue.len())
            .finish()
    }
}
