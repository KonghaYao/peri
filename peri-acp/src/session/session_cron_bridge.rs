//! Session-scoped cron bridge: MCP trigger resource → Host continuation scheduler.
//!
//! Lives exactly as long as its owning [`crate::session::AcpSession`]: created
//! at the session publication boundary, dropped (task aborted) when the
//! session closes. Survives turn end and session/cancel.

use std::sync::Arc;

use peri_acp_types::cron::CronContinuationRequest;
use peri_acp_types::ports::McpPoolPort;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub struct SessionCronBridge {
    handle: tokio::task::JoinHandle<()>,
    shutdown: CancellationToken,
}

impl SessionCronBridge {
    /// Read the MCP owner's outbox and forward complete triggers to
    /// Host's unified continuation scheduler. The bridge never writes the inbox:
    /// approval must happen before a trigger becomes model input.
    pub fn start(
        session_id: String,
        inbox: peri_acp_types::session::MessageQueue,
        pool: Arc<dyn McpPoolPort>,
        continuation_tx: mpsc::UnboundedSender<CronContinuationRequest>,
    ) -> Self {
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();
        let handle = peri_agent::session::cron_events::forward_cron_triggers(
            pool,
            shutdown_clone,
            move |trigger| {
                continuation_tx
                    .send(CronContinuationRequest {
                        session_id: session_id.clone(),
                        inbox: inbox.clone(),
                        trigger,
                    })
                    .is_ok()
            },
        );
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
