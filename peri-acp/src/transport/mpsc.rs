//! In-memory ACP transport using tokio mpsc channels.
//!
//! `mpsc_transport_pair()` creates a connected pair of transports — one for the
//! ACP server side and one for the client (TUI) side. Messages flow through two
//! pairs of unbounded channels.
//!
//! Each transport spawns a background pump task that continuously reads incoming
//! messages and dispatches responses to the pending request map, so `send_request`
//! can await the oneshot channel without deadlocking.

use std::{
    collections::HashMap,
    sync::{atomic::AtomicI64, Arc},
};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

use super::{
    router::RequestRouter,
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
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let pump_router = router.clone();

        // Background pump: dispatches Response messages to the pending map,
        // forwards Requests and Notifications to incoming_rx.
        tokio::spawn(async move {
            let mut rx = server_rx;
            while let Some(msg) = rx.recv().await {
                let incoming = channel_to_incoming(msg);
                if !pump_router.dispatch(&incoming).await {
                    let _ = incoming_tx.send(incoming);
                }
            }
        });

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
        let (id_num, response_rx) = self.router.register().await;
        let id = RequestId::Number(id_num);

        self.client_tx
            .send(ChannelMessage::Request {
                id,
                method: method.to_string(),
                params,
            })
            .map_err(|_| AcpError::new(-32603, "Transport closed"))?;

        response_rx
            .await
            .map_err(|_| AcpError::new(-32603, "Request cancelled"))?
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.client_tx
            .send(ChannelMessage::Notification {
                method: method.to_string(),
                params,
            })
            .map_err(|_| AcpError::new(-32603, "Transport closed"))
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.client_tx
            .send(ChannelMessage::Response { id, result })
            .map_err(|_| AcpError::new(-32603, "Transport closed"))
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
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let pump_router = router.clone();

        // Background pump
        tokio::spawn(async move {
            let mut rx = client_rx;
            while let Some(msg) = rx.recv().await {
                let incoming = channel_to_incoming(msg);
                if !pump_router.dispatch(&incoming).await {
                    let _ = incoming_tx.send(incoming);
                }
            }
        });

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
        let (id_num, response_rx) = self.router.register().await;
        let id = RequestId::Number(id_num);

        self.server_tx
            .send(ChannelMessage::Request {
                id,
                method: method.to_string(),
                params,
            })
            .map_err(|_| AcpError::new(-32603, "Transport closed"))?;

        response_rx
            .await
            .map_err(|_| AcpError::new(-32603, "Request cancelled"))?
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.server_tx
            .send(ChannelMessage::Notification {
                method: method.to_string(),
                params,
            })
            .map_err(|_| AcpError::new(-32603, "Transport closed"))
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.server_tx
            .send(ChannelMessage::Response { id, result })
            .map_err(|_| AcpError::new(-32603, "Transport closed"))
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

    let pending = Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicI64::new(1));

    let client_router = RequestRouter::new_shared(pending.clone(), next_id.clone());
    let server_router = RequestRouter::new_shared(pending, next_id);

    let client = MpscClientTransport::new(client_tx, server_rx, client_router);
    let server = MpscServerTransport::new(client_rx, server_tx, server_router);

    (client, server)
}

#[cfg(test)]
#[path = "mpsc_test.rs"]
mod tests;
