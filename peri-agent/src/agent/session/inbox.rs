//! SessionInbox — await-wake wrapper around v2 MessageQueue
//!
//! ## Purpose
//!
//! The v2 [`MessageQueue`] already has internal `Notify` + `wait_for_message()`, but
//! its API is "raw" — producers push and the consumer must manually drain. This module
//! adds a semantic layer:
//!
//! - **Producers** use [`InboxHandle`] — a cloneable handle that pushes and wakes.
//! - **Consumer** (ACP executor's `run_session_loop`) uses [`SessionInbox::await_wake`]
//!   to block during IDLE until a wake-able message arrives.
//!
//! ## Invariants
//!
//! 1. `await_wake` is **non-destructive** — it does NOT drain. `stages/receive.rs`
//!    uses `drain_all` in the loop body; `drain_for_receive` and `drain_for_end`
//!    remain as public APIs for external flush paths (executor helpers, tests).
//! 2. Pushers from Agent/ACP layer use `InboxHandle` (cloneable, `Send + Sync`).
//! 3. TUI should NOT have access to `InboxHandle` — all async events
//!    (cron/channel/workflow/bg_results) flow through Agent/ACP layer →
//!    `InboxHandle::push` → `MessageQueue` → `await_wake` → Receive's `drain_all`.
//!
//! ## Two-phase async loop
//!
//! ```text
//! Agent running (loading=true):
//!   async event → push to queue → stages/receive.rs drain_all → current/next turn
//!
//! Agent idle (loading=false):
//!   async event → push to queue → await_wake returns → run_session_loop starts new turn
//!    → Receive stage drain_all consumes the message
//! ```

use std::sync::Arc;

use crate::messages::BaseMessage;
use crate::session::{MessageQueue, MessageSource, QueuedMessage};

/// Wraps the existing v2 MessageQueue with an async await-wake mechanism.
///
/// During ReAct loop, `stages/receive.rs` calls `drain_all`
/// to consume pending messages — no wake needed (loop is already spinning).
///
/// During IDLE (between ReAct loops), the ACP executor calls [`await_wake`](Self::await_wake)
/// which blocks until a new Prompt/Defer is enqueued, then the loop resumes.
pub struct SessionInbox {
    queue: Arc<MessageQueue>,
    /// Dedicated notify for await_wake — separate from queue's internal notify
    /// to avoid spurious wakeups when Info messages are pushed.
    wake: Arc<tokio::sync::Notify>,
}

impl SessionInbox {
    /// Create a new SessionInbox wrapping the given queue.
    ///
    /// The queue is typically the session-level shared instance passed through
    /// `Session::new_with_cancel_and_queue`.
    pub fn new(queue: Arc<MessageQueue>) -> Self {
        Self {
            queue,
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Block until the inbox has at least one wake-able message (Prompt or Defer).
    ///
    /// Called by ACP executor's `run_session_loop` when the previous iteration ends
    /// with `should_continue = false` (no more messages to process).
    ///
    /// ## Non-destructive
    ///
    /// This method does NOT drain any messages. The actual consumption happens in
    /// `stages/receive.rs` via `drain_all`; `drain_for_receive` and `drain_for_end`
    /// remain available for external flush callers.
    ///
    /// ## Spurious wakeup guard
    ///
    /// After waking, we re-check `has_wake_up()`. If only Info messages arrived
    /// (which don't wake the loop), we go back to waiting. This prevents the executor
    /// from spinning on Info-only notifications.
    pub async fn await_wake(&self) {
        // Fast path: if already pending, return immediately
        if self.queue.has_wake_up() {
            return;
        }
        loop {
            self.wake.notified().await;
            // Guard against spurious wakeups: only wake on Prompt/Defer
            if self.queue.has_wake_up() {
                return;
            }
        }
    }

    /// Get a cloneable handle for producers.
    ///
    /// Producers (cron owner, channel owner, async router for bg_results, etc.)
    /// use this handle to push messages and wake the idle executor.
    pub fn handle(&self) -> InboxHandle {
        InboxHandle {
            queue: Arc::clone(&self.queue),
            wake: Arc::clone(&self.wake),
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
/// Producers (cron_owner, channel_owner, async_router for bg_results) hold this
/// handle to push messages and wake the idle executor. The handle is `Send + Sync`
/// and cheaply cloneable — safe to store in long-lived components.
///
/// TUI should NOT have access to this handle.
#[derive(Clone)]
pub struct InboxHandle {
    queue: Arc<MessageQueue>,
    wake: Arc<tokio::sync::Notify>,
}

impl InboxHandle {
    /// Push a Prompt message (user input or external request) and wake the executor.
    ///
    /// Prompt messages are consumed by `drain_all` during the Receive stage
    /// and wake the loop.
    pub fn push_prompt(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::prompt(source, message));
        self.wake.notify_one();
    }

    /// Push a Defer message (SubAgent complete, Cron trigger, bg result) and wake.
    ///
    /// In RCRA, Defer messages are consumed by `drain_all` during the Receive stage.
    /// They are also detectable via `drain_for_end` for external callers.
    pub fn push_defer(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::defer(source, message));
        self.wake.notify_one();
    }

    /// Push an Info message (system reminder, hook injection) — does NOT wake.
    ///
    /// Info messages are consumed by `drain_all` (in the loop) or `drain_for_receive`
    /// (external flush paths), but never wake the loop.
    /// They must be carried out by a Prompt message arriving later.
    pub fn push_info(&self, source: MessageSource, message: BaseMessage) {
        // Intentionally no wake.notify_one() — Info does not wake the loop
        self.queue.push(QueuedMessage::info(source, message));
    }

    /// Push an arbitrary QueuedMessage and conditionally wake.
    ///
    /// Wakes only if the message kind is Prompt or Defer (i.e., `kind.wakes_up()`).
    pub fn push(&self, msg: QueuedMessage) {
        let should_wake = msg.kind.wakes_up();
        self.queue.push(msg);
        if should_wake {
            self.wake.notify_one();
        }
    }

    /// Batch push messages; wakes once if any message is wake-able.
    pub fn push_batch(&self, msgs: Vec<QueuedMessage>) {
        if msgs.is_empty() {
            return;
        }
        let should_wake = msgs.iter().any(|m| m.kind.wakes_up());
        self.queue.push_batch(msgs);
        if should_wake {
            self.wake.notify_one();
        }
    }
}

impl std::fmt::Debug for InboxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxHandle")
            .field("queue_len", &self.queue.len())
            .finish()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "inbox_test.rs"]
mod tests;
