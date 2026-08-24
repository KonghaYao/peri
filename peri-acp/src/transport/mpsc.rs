//! In-memory ACP transport using tokio mpsc channels.
//!
//! `mpsc_transport_pair()` creates a connected pair of transports — one for the
//! ACP server side and one for the client (TUI) side. Messages flow through two
//! pairs of unbounded channels.
//!
//! Each transport spawns a background pump task that continuously reads incoming
//! messages and dispatches responses to the pending request map, so `send_request`
//! can await the oneshot channel without deadlocking.

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::{
    router::{transport_closed_error, RequestRouter},
    types::{AcpError, IncomingMessage, RequestId},
    AcpTransport,
};

// ---------- internal channel message types ----------

#[derive(Debug)]
enum ChannelMessage {
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: RequestId,
        result: Result<Value, AcpError>,
    },
}

// ---------- shared pending map ----------

/// Convert an internal `ChannelMessage` into a public `IncomingMessage` for dispatch.
fn channel_to_incoming(msg: ChannelMessage) -> IncomingMessage {
    match msg {
        ChannelMessage::Request { id, method, params } => {
            IncomingMessage::Request { id, method, params }
        }
        ChannelMessage::Notification { method, params } => {
            IncomingMessage::Notification { method, params }
        }
        ChannelMessage::Response { id, result } => IncomingMessage::Response { id, result },
    }
}

fn spawn_pump(
    mut receiver: mpsc::UnboundedReceiver<ChannelMessage>,
    router: RequestRouter,
) -> mpsc::UnboundedReceiver<IncomingMessage> {
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                message = receiver.recv() => {
                    let Some(message) = message else {
                        router.close();
                        break;
                    };
                    let incoming = channel_to_incoming(message);
                    if !router.dispatch(&incoming) && incoming_tx.send(incoming).is_err() {
                        router.close();
                        break;
                    }
                }
                () = router.wait_closed() => break,
            }
        }
    });
    incoming_rx
}

fn send_or_close(
    sender: &mpsc::UnboundedSender<ChannelMessage>,
    router: &RequestRouter,
    message: ChannelMessage,
) -> Result<(), AcpError> {
    router.ensure_open()?;
    sender.send(message).map_err(|_| {
        router.close();
        transport_closed_error()
    })
}

// ---------- MpscClientTransport ----------

/// Client-side (TUI) transport.
pub struct MpscClientTransport {
    /// Sends client → server messages.
    client_tx: mpsc::UnboundedSender<ChannelMessage>,
    /// Receives processed incoming messages from the pump.
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<IncomingMessage>>,
    /// Shared request-response router.
    router: RequestRouter,
}

impl MpscClientTransport {
    fn new(
        client_tx: mpsc::UnboundedSender<ChannelMessage>,
        server_rx: mpsc::UnboundedReceiver<ChannelMessage>,
        router: RequestRouter,
    ) -> Self {
        let incoming_rx = spawn_pump(server_rx, router.clone());

        Self {
            client_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            router,
        }
    }
}

#[async_trait]
impl AcpTransport for MpscClientTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let pending = self.router.register()?;
        let id = RequestId::Number(pending.id());

        send_or_close(
            &self.client_tx,
            &self.router,
            ChannelMessage::Request {
                id,
                method: method.to_string(),
                params,
            },
        )?;

        pending.wait().await
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        send_or_close(
            &self.client_tx,
            &self.router,
            ChannelMessage::Notification {
                method: method.to_string(),
                params,
            },
        )
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        send_or_close(
            &self.client_tx,
            &self.router,
            ChannelMessage::Response { id, result },
        )
    }
}

// ---------- MpscServerTransport ----------

/// Server-side (ACP) transport.
pub struct MpscServerTransport {
    /// Sends server → client messages.
    server_tx: mpsc::UnboundedSender<ChannelMessage>,
    /// Receives processed incoming messages from the pump.
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<IncomingMessage>>,
    /// Shared request-response router.
    router: RequestRouter,
}

impl MpscServerTransport {
    fn new(
        client_rx: mpsc::UnboundedReceiver<ChannelMessage>,
        server_tx: mpsc::UnboundedSender<ChannelMessage>,
        router: RequestRouter,
    ) -> Self {
        let incoming_rx = spawn_pump(client_rx, router.clone());

        Self {
            server_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            router,
        }
    }
}

#[async_trait]
impl AcpTransport for MpscServerTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let pending = self.router.register()?;
        let id = RequestId::Number(pending.id());

        send_or_close(
            &self.server_tx,
            &self.router,
            ChannelMessage::Request {
                id,
                method: method.to_string(),
                params,
            },
        )?;

        pending.wait().await
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        send_or_close(
            &self.server_tx,
            &self.router,
            ChannelMessage::Notification {
                method: method.to_string(),
                params,
            },
        )
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        send_or_close(
            &self.server_tx,
            &self.router,
            ChannelMessage::Response { id, result },
        )
    }
}

// ---------- factory ----------

/// Create a connected pair of in-memory ACP transports.
///
/// Returns `(client, server)` where:
/// - `client` is used by the TUI / ACP client side
/// - `server` is used by the ACP session manager side
///
/// Each transport spawns a background pump task for processing incoming
/// messages, so the pair must be created within a tokio runtime.
pub fn mpsc_transport_pair() -> (MpscClientTransport, MpscServerTransport) {
    let (client_tx, client_rx) = mpsc::unbounded_channel();
    let (server_tx, server_rx) = mpsc::unbounded_channel();

    let client_router = RequestRouter::new();
    let server_router = client_router.clone();

    let client = MpscClientTransport::new(client_tx, server_rx, client_router);
    let server = MpscServerTransport::new(client_rx, server_tx, server_router);

    (client, server)
}

#[cfg(test)]
#[path = "mpsc_test.rs"]
mod tests;
