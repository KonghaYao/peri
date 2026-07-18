//! ChannelOwner — Agent-owned channel notification bridge
//!
//! Owns the channel notification receiver loop and bridges channel messages directly
//! into the [`SessionInbox`] via [`InboxHandle`], bypassing the TUI.
//!
//! ## Architecture
//!
//! Previously, `channel_notification_rx` lived in the TUI layer (`MessageState`).
//! Channel notifications flowed through `poll_channel_notifications` which drained
//! the receiver every frame and injected into the v2 MessageQueue as a Defer with
//! `MessageSource::ChannelMessage`. The Agent layer never saw `ChannelNotification`
//! directly.
//!
//! With `ChannelOwner`, the Agent/ACP layer owns the receiver and its forwarding loop.
//! On notification, the message is pushed directly into the inbox as a Defer with
//! `MessageSource::ChannelMessage`. The inbox's wake mechanism ensures the idle
//! executor resumes promptly.
//!
//! ## Circular dependency avoidance
//!
//! `peri-agent` cannot depend on `peri-middlewares` (which depends on `peri-agent`).
//! However, `ChannelNotification` is defined in `peri-agent/src/interaction/channel_types.rs`,
//! so no bridge is needed — `ChannelOwner` receives the structured type directly.
//!
//! ## Usage (in peri-acp)
//!
//! ```text
//! // 1. Obtain the channel_notification_rx from the channel broker
//! let channel_rx = channel_broker.subscribe(/* ... */);
//!
//! // 2. Create ChannelOwner and start
//! let mut channel_owner = ChannelOwner::new();
//! channel_owner.start(channel_rx, inbox_handle, cancel_token);
//! ```

use tokio::sync::mpsc;

use crate::agent::session::inbox::InboxHandle;
use crate::interaction::channel_types::ChannelNotification;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::{MessageSource, QueuedMessage};

/// Agent-owned channel notification bridge.
///
/// Spawns a tokio task that:
/// 1. Receives `ChannelNotification` from the channel broker.
/// 2. Formats each notification as a `<system-reminder><channel ...>` block.
/// 3. Pushes each formatted message into the inbox as a Defer + `ChannelMessage` source.
///
/// The channel broker itself is external — `ChannelOwner` only owns the
/// notification-to-inbox forwarding.
pub struct ChannelOwner {
    /// Handle to the spawned notification-forwarding task.
    /// `None` before [`start`](Self::start) is called.
    handle_task: Option<tokio::task::JoinHandle<()>>,
}

impl ChannelOwner {
    /// Create a new (not yet started) ChannelOwner.
    pub fn new() -> Self {
        Self { handle_task: None }
    }

    /// Spawn the notification-forwarding loop.
    ///
    /// Receives `ChannelNotification` from `channel_rx` and pushes each one into
    /// the inbox as `QueuedMessage::defer(MessageSource::ChannelMessage, ...)`.
    ///
    /// Each notification is formatted as:
    /// ```text
    /// <system-reminder><channel source="..." chat_id="...">...</channel></system-reminder>
    /// ```
    ///
    /// The loop terminates when either:
    /// - `shutdown` is cancelled (session tear-down), or
    /// - `channel_rx` is closed (broker dropped).
    ///
    /// # Parameters
    ///
    /// - `channel_rx`: Unbounded receiver of channel notifications. Each received
    ///   notification is a `ChannelNotification` from an external channel (WeChat, Slack, etc.).
    /// - `inbox`: Cloneable handle to the session inbox.
    /// - `shutdown`: Cancellation token tied to the session lifetime.
    pub fn start(
        &mut self,
        mut channel_rx: mpsc::UnboundedReceiver<ChannelNotification>,
        inbox: InboxHandle,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        tracing::debug!("channel_owner: shutdown signal received, stopping");
                        break;
                    }
                    notif = channel_rx.recv() => {
                        match notif {
                            Some(notif) => {
                                let payload = format!(
                                    "<system-reminder><channel source=\"{}\" chat_id=\"{}\">{}</channel></system-reminder>",
                                    notif.source, notif.chat_id, notif.text
                                );
                                let message = BaseMessage::human(
                                    MessageContent::text(payload),
                                );
                                inbox.push(QueuedMessage::defer(
                                    MessageSource::ChannelMessage,
                                    message,
                                ));
                                tracing::info!(
                                    source = %notif.source,
                                    chat_id = %notif.chat_id,
                                    "channel_owner: notification pushed to inbox"
                                );
                            }
                            None => {
                                // channel_rx closed (broker dropped)
                                tracing::debug!("channel_owner: channel_rx closed, stopping");
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

impl Default for ChannelOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ChannelOwner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for ChannelOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelOwner")
            .field("running", &self.handle_task.is_some())
            .finish()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "channel_owner_test.rs"]
mod tests;
