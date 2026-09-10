//! Session 生命周期内的 cron 触发转发任务及其 Drop 终止。

use super::{InboxHandle, MessageSource, QueuedMessage};
use crate::messages::BaseMessage;
use std::sync::Arc;

// ─── CronOwner ───────────────────────────────────────────────────────────────

/// Agent-owned cron evaluation bridge。
///
/// Spawns a tokio task that receives trigger prompts from the channel and
/// pushes each prompt into the inbox as a Defer + `CronTrigger` source。
///
/// 循环依赖规避：本模块不 import `CronScheduler` / `CronTrigger`
/// （peri-middlewares 类型），只接收 `UnboundedReceiver<String>`——
/// 从 `CronTrigger.prompt` 到本通道的桥接在装配点（peri-acp host）完成。
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
        mut trigger_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
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
                                    crate::messages::MessageContent::text(format!(
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
