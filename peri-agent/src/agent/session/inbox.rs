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
//! 1. `await_wake` is **non-destructive** — it does NOT drain. `stages/end.rs`
//!    `drain_for_receive` / `drain_for_end` still do the actual draining.
//! 2. Pushers from Agent/ACP layer use `InboxHandle` (cloneable, `Send + Sync`).
//! 3. TUI should NOT have access to `InboxHandle` — TUI loses its `drain_for_end`
//!    responsibility. All async events (cron/channel/workflow/bg_results) flow through
//!    Agent/ACP layer → `InboxHandle::push` → `MessageQueue` → `await_wake` / `drain_for_end`.
//!
//! ## Two-phase async loop
//!
//! ```text
//! Agent running (loading=true):
//!   async event → push to queue → stages/end.rs drain_for_end → next turn
//!
//! Agent idle (loading=false):
//!   async event → push to queue → await_wake returns → run_session_loop starts new turn
//! ```

use std::sync::Arc;

use crate::messages::BaseMessage;
use crate::session::{MessageQueue, MessageSource, QueuedMessage};

/// Wraps the existing v2 MessageQueue with an async await-wake mechanism.
///
/// During ReAct loop, `stages/end.rs` calls `drain_for_end` / `drain_for_receive`
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
    /// `stages/end.rs` via `drain_for_end` or `stages/receive.rs` via `drain_for_receive`.
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
    /// Prompt messages are consumed by `drain_for_receive` during the next turn
    /// and can wake the loop via `drain_for_end`.
    pub fn push_prompt(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::prompt(source, message));
        self.wake.notify_one();
    }

    /// Push a Defer message (SubAgent complete, Cron trigger, bg result) and wake.
    ///
    /// Defer messages are skipped by `drain_for_receive` (preserved in queue)
    /// and consumed + woken by `drain_for_end` when the loop reaches the End stage.
    pub fn push_defer(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::defer(source, message));
        self.wake.notify_one();
    }

    /// Push an Info message (system reminder, hook injection) — does NOT wake.
    ///
    /// Info messages are consumed by `drain_for_receive` but never wake the loop.
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
mod tests {
    use super::*;
    use crate::messages::MessageContent;
    use std::time::Duration;

    fn make_msg(text: &str) -> BaseMessage {
        BaseMessage::human(MessageContent::text(text.to_string()))
    }

    #[test]
    fn test_inbox_handle_push_prompt_wakes() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        handle.push_prompt(MessageSource::UserInput, make_msg("hello"));
        assert!(inbox.queue().has_wake_up());
        assert_eq!(inbox.queue().len(), 1);
    }

    #[test]
    fn test_inbox_handle_push_defer_wakes() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        handle.push_defer(MessageSource::SubAgentComplete, make_msg("done"));
        assert!(inbox.queue().has_wake_up());
    }

    #[test]
    fn test_inbox_handle_push_info_does_not_wake() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        handle.push_info(MessageSource::SystemInjected, make_msg("info"));
        assert!(!inbox.queue().has_wake_up());
        // Info is still in the queue
        assert_eq!(inbox.queue().len(), 1);
    }

    #[test]
    fn test_inbox_handle_push_arbitrary_conditional_wake() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        // Info via push() — no wake
        handle.push(QueuedMessage::info(
            MessageSource::SystemInjected,
            make_msg("info"),
        ));
        assert!(!inbox.queue().has_wake_up());

        // Prompt via push() — wakes
        handle.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            make_msg("prompt"),
        ));
        assert!(inbox.queue().has_wake_up());
    }

    #[test]
    fn test_inbox_handle_batch_wakes_on_any_prompt_or_defer() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        // Batch of only Info — no wake
        handle.push_batch(vec![QueuedMessage::info(
            MessageSource::SystemInjected,
            make_msg("info1"),
        )]);
        assert!(!inbox.queue().has_wake_up());

        // Batch with one Prompt — wakes
        handle.push_batch(vec![
            QueuedMessage::info(MessageSource::SystemInjected, make_msg("info2")),
            QueuedMessage::prompt(MessageSource::UserInput, make_msg("prompt")),
        ]);
        assert!(inbox.queue().has_wake_up());
    }

    #[test]
    fn test_inbox_handle_batch_empty_no_op() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        handle.push_batch(vec![]);
        assert!(inbox.queue().is_empty());
    }

    #[test]
    fn test_inbox_handle_clone_independence() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle1 = inbox.handle();
        let handle2 = inbox.handle();

        handle1.push_prompt(MessageSource::UserInput, make_msg("from h1"));
        handle2.push_defer(MessageSource::CronTrigger, make_msg("from h2"));

        // Both handles write to the same underlying queue
        assert_eq!(inbox.queue().len(), 2);
    }

    #[tokio::test]
    async fn test_await_wake_returns_immediately_when_pending() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        // Push before await — should return immediately
        handle.push_prompt(MessageSource::UserInput, make_msg("already here"));

        // Should not hang
        tokio::time::timeout(Duration::from_millis(100), inbox.await_wake())
            .await
            .expect("await_wake should return immediately when pending");
    }

    #[tokio::test]
    async fn test_await_wake_blocks_until_prompt() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        let inbox_clone = inbox; // move into async block

        let handle_async = handle.clone();
        let h = tokio::spawn(async move {
            // Wait a bit then push
            tokio::time::sleep(Duration::from_millis(50)).await;
            handle_async.push_prompt(MessageSource::UserInput, make_msg("wake me"));
        });

        // await_wake should block until the push
        tokio::time::timeout(Duration::from_secs(1), inbox_clone.await_wake())
            .await
            .expect("await_wake should return after push");

        h.await.unwrap();
    }

    #[tokio::test]
    async fn test_await_wake_ignores_info_only() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        let inbox_clone = inbox;
        let handle_async = handle.clone();

        let h = tokio::spawn(async move {
            // Push Info (should NOT wake)
            handle_async.push_info(MessageSource::SystemInjected, make_msg("info"));
            // Wait then push Prompt (should wake)
            tokio::time::sleep(Duration::from_millis(50)).await;
            handle_async.push_prompt(MessageSource::UserInput, make_msg("now wake"));
        });

        // await_wake should NOT return on Info, only on Prompt
        tokio::time::timeout(Duration::from_secs(1), inbox_clone.await_wake())
            .await
            .expect("await_wake should return after Prompt, not Info");

        h.await.unwrap();
    }

    #[tokio::test]
    async fn test_await_wake_non_destructive() {
        // await_wake should NOT consume messages
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();

        handle.push_prompt(MessageSource::UserInput, make_msg("preserve me"));
        inbox.await_wake().await;

        // Message should still be in the queue
        assert_eq!(inbox.queue().len(), 1, "await_wake should not drain");
    }
}
