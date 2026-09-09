//! Session-scoped cron bridge: CronSchedulerPort → Host continuation scheduler.
//!
//! Lives exactly as long as its owning [`crate::session::AcpSession`]: created
//! at the session publication boundary, dropped (task aborted) when the
//! session closes. Survives turn end and session/cancel.

use std::sync::Arc;

use peri_acp_types::cron::{CronContinuationRequest, CronSchedulerPort};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub struct SessionCronBridge {
    handle: tokio::task::JoinHandle<()>,
    shutdown: CancellationToken,
}

impl SessionCronBridge {
    /// Subscribe to the scheduler exactly once and forward complete triggers to
    /// Host's unified continuation scheduler. The bridge never writes the inbox:
    /// approval must happen before a trigger becomes model input.
    pub fn start(
        session_id: String,
        scheduler: &Arc<dyn CronSchedulerPort>,
        continuation_tx: mpsc::UnboundedSender<CronContinuationRequest>,
    ) -> Self {
        let mut trigger_rx = scheduler.subscribe();
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_clone.cancelled() => break,
                    trigger = trigger_rx.recv() => match trigger {
                        Some(trigger) => {
                            if continuation_tx.send(CronContinuationRequest {
                                session_id: session_id.clone(),
                                trigger,
                            }).is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
        });
        Self { handle, shutdown }
    }

    /// Graceful stop: cancel token then abort as a backstop.
    pub fn shutdown(&mut self) {
        self.shutdown.cancel();
        self.handle.abort();
    }
}

impl Drop for SessionCronBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}
