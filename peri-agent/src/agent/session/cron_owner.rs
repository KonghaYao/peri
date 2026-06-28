//! CronOwner — Agent-owned cron scheduler bridge
//!
//! Owns the cron evaluation loop and bridges cron triggers directly into the
//! [`SessionInbox`] via [`InboxHandle`], bypassing the TUI.
//!
//! ## Architecture
//!
//! Previously, `CronScheduler` lived in the TUI layer. Triggers flowed through
//! `trigger_rx` (TUI-owned) → `poll_cron_triggers` → `v2 MessageQueue`. This
//! meant the Agent layer had zero knowledge of cron.
//!
//! With `CronOwner`, the Agent/ACP layer owns the scheduler and its tick loop.
//! On trigger, the prompt is pushed directly into the inbox as a Defer message
//! with `MessageSource::CronTrigger`. The inbox's wake mechanism ensures the
//! idle executor resumes promptly.
//!
//! ## Circular dependency avoidance
//!
//! `peri-agent` cannot depend on `peri-middlewares` (which depends on `peri-agent`).
//! Therefore, `CronOwner` does not import `CronScheduler` or `CronTrigger` directly.
//! Instead, it receives a `tokio::sync::mpsc::UnboundedReceiver<String>` — the prompt
//! text to inject on each trigger. The bridge from `CronTrigger.prompt` to this
//! channel is established at the construction site in `peri-acp` (which depends
//! on both crates).
//!
//! ## Usage (in peri-acp)
//!
//! ```text
//! // 1. Create CronScheduler (from peri_middlewares::cron)
//! let (trigger_tx, trigger_rx) = mpsc::unbounded_channel();
//! let scheduler = CronScheduler::new(trigger_tx);
//!
//! // 2. Bridge CronTrigger → String channel for CronOwner
//! let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
//! // Spawn a small bridge task: CronTrigger → prompt_tx.send(trigger.prompt)
//!
//! // 3. Create CronOwner and start
//! let mut cron_owner = CronOwner::new();
//! cron_owner.start(prompt_rx, inbox_handle, cancel_token);
//! ```

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::session::inbox::InboxHandle;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::{MessageSource, QueuedMessage};

/// Agent-owned cron evaluation bridge.
///
/// Spawns a tokio task that:
/// 1. Ticks the scheduler every second (via external bridge).
/// 2. Receives trigger prompts from the channel.
/// 3. Pushes each prompt into the inbox as a Defer + `CronTrigger` source.
///
/// The tick loop itself is external — `CronOwner` only owns the trigger-to-inbox
/// forwarding. The actual `CronScheduler::tick()` is called by a separate task
/// (or the same bridge task) in `peri-acp`.
pub struct CronOwner {
    /// Handle to the spawned trigger-forwarding task.
    /// `None` before [`start`](Self::start) is called.
    handle_task: Option<tokio::task::JoinHandle<()>>,
}

impl CronOwner {
    /// Create a new (not yet started) CronOwner.
    pub fn new() -> Self {
        Self { handle_task: None }
    }

    /// Spawn the trigger-forwarding loop.
    ///
    /// Receives prompt strings from `trigger_rx` and pushes each one into
    /// the inbox as `QueuedMessage::defer(MessageSource::CronTrigger, ...)`.
    ///
    /// The loop terminates when either:
    /// - `shutdown` is cancelled (session tear-down), or
    /// - `trigger_rx` is closed (scheduler dropped).
    ///
    /// # Parameters
    ///
    /// - `trigger_rx`: Unbounded receiver of prompt strings. Each received
    ///   string is the prompt from a fired `CronTrigger`.
    /// - `inbox`: Cloneable handle to the session inbox.
    /// - `shutdown`: Cancellation token tied to the session lifetime (Arc-shared clone).
    pub fn start(
        &mut self,
        mut trigger_rx: mpsc::UnboundedReceiver<String>,
        inbox: InboxHandle,
        shutdown: Arc<tokio_util::sync::CancellationToken>,
    ) {
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        tracing::debug!("cron_owner: shutdown signal received, stopping");
                        break;
                    }
                    prompt = trigger_rx.recv() => {
                        match prompt {
                            Some(prompt) => {
                                let message = BaseMessage::human(
                                    MessageContent::text(format!(
                                        "<goal-message>Cron triggered: {}</goal-message>",
                                        prompt
                                    )),
                                );
                                inbox.push(QueuedMessage::defer(
                                    MessageSource::CronTrigger,
                                    message,
                                ));
                                tracing::debug!(prompt = %prompt, "cron_owner: trigger pushed to inbox");
                            }
                            None => {
                                // trigger_rx closed (scheduler dropped)
                                tracing::debug!("cron_owner: trigger_rx closed, stopping");
                                break;
                            }
                        }
                    }
                }
            }
        });
        self.handle_task = Some(handle);
    }

    /// Abort the background task if running.
    ///
    /// Called during session tear-down to ensure clean shutdown even if the
    /// cancellation token has not yet fired.
    pub fn shutdown(&mut self) {
        if let Some(handle) = self.handle_task.take() {
            handle.abort();
        }
    }
}

impl Default for CronOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CronOwner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for CronOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronOwner")
            .field("running", &self.handle_task.is_some())
            .finish()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::agent::session::inbox::SessionInbox;
    use crate::session::MessageQueue;

    #[tokio::test]
    async fn test_cron_owner_forwards_trigger_to_inbox() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let inbox_handle = inbox.handle();

        let (tx, rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

        let mut owner = CronOwner::new();
        owner.start(rx, inbox_handle, shutdown.clone());

        // Send a trigger prompt
        tx.send("check deploy status".to_string()).unwrap();

        // Give the task time to process
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should have one defer message in the queue
        assert_eq!(inbox.queue().len(), 1);

        // Verify it's a CronTrigger defer (drain_for_end returns Some when Defer present)
        let drained = inbox
            .queue()
            .drain_for_end()
            .expect("Defer should wake drain_for_end");
        let msg = drained.into_iter().next().unwrap();
        assert_eq!(msg.source, MessageSource::CronTrigger);

        shutdown.cancel();
        owner.shutdown();
    }

    #[tokio::test]
    async fn test_cron_owner_stops_on_shutdown() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let inbox_handle = inbox.handle();

        let (tx, rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

        let mut owner = CronOwner::new();
        owner.start(rx, inbox_handle, shutdown.clone());

        // Cancel should stop the loop
        shutdown.cancel();

        // Give time for the task to notice cancellation
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Sending after shutdown should fail — receiver was dropped when task ended.
        // This proves the task actually stopped (rx no longer alive).
        assert!(
            tx.send("should not arrive".to_string()).is_err(),
            "sender should fail after receiver task ends"
        );

        // Queue should be empty — no trigger was sent before shutdown.
        assert!(inbox.queue().is_empty());

        owner.shutdown();
    }

    #[tokio::test]
    async fn test_cron_owner_stops_when_rx_closed() {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let inbox_handle = inbox.handle();

        let (tx, rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

        let mut owner = CronOwner::new();
        owner.start(rx, inbox_handle, shutdown);

        // Drop the sender — closes the channel
        drop(tx);

        // Task should exit on its own
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Queue should be empty
        assert!(inbox.queue().is_empty());
    }

    #[tokio::test]
    async fn test_cron_owner_default_and_debug() {
        let owner = CronOwner::default();
        assert!(!format!("{:?}", owner).contains("running: true"));

        let mut owner = CronOwner::new();
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let (_tx, rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(tokio_util::sync::CancellationToken::new());

        owner.start(rx, inbox.handle(), shutdown);
        assert!(format!("{:?}", owner).contains("running: true"));

        owner.shutdown();
        assert!(!format!("{:?}", owner).contains("running: true"));
    }
}
