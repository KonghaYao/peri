//! Session-scoped cron bridge: CronSchedulerPort → CronOwner → session inbox.
//!
//! Lives exactly as long as its owning [`crate::session::AcpSession`]: created
//! lazily on the first turn of a session, dropped (task aborted) when the
//! session closes. Survives turn end and session/cancel.

use std::sync::Arc;

use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::session::CronOwner;
use peri_acp_types::session::InboxHandle;
use tokio_util::sync::CancellationToken;

pub struct SessionCronBridge {
    owner: CronOwner,
    shutdown: CancellationToken,
}

impl SessionCronBridge {
    /// Subscribe to the scheduler exactly once and start the forwarding task.
    /// `inbox` MUST be the session-level inbox handle (same wake Notify as the
    /// executor's `await_wake`) — this fixes the per-turn wake mismatch.
    ///
    /// 桥接任务（CronTrigger → String）必须在此完成：peri-agent 不能依赖
    /// peri-middlewares（循环依赖），`CronOwner::start` 只收
    /// `UnboundedReceiver<String>`（cron_owner.rs:87-91），而
    /// `subscribe()` 返回 `UnboundedReceiver<CronTrigger>`（cron/mod.rs:71-75）。
    /// 结构照搬 builder.rs:925-951。
    pub fn start(scheduler: &Arc<dyn CronSchedulerPort>, inbox: InboxHandle) -> Self {
        let mut trigger_rx = scheduler.subscribe(); // UnboundedReceiver<CronTrigger>
        let shutdown = CancellationToken::new();
        let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_clone.cancelled() => break,
                    trigger = trigger_rx.recv() => match trigger {
                        Some(t) => { if prompt_tx.send(t.prompt).is_err() { break; } }
                        None => break, // scheduler dropped
                    },
                }
            }
        });
        let mut owner = CronOwner::new();
        // [TRAP] 必须 clone：Arc::new(shutdown) 会 move，后续 Self { owner, shutdown } 编译失败（E0382）
        owner.start(prompt_rx, inbox, Arc::new(shutdown.clone()));
        Self { owner, shutdown }
    }

    /// Graceful stop: cancel token (task exits via select) then abort (backstop).
    pub fn shutdown(&mut self) {
        self.shutdown.cancel();
        self.owner.shutdown();
    }
}

impl Drop for SessionCronBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}
