//! RequestRouter — shared pending request map + response dispatch for all transports.
//!
//! Extracted from duplicated logic in `mpsc.rs` and `stdio.rs`.

use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
};
use tokio::sync::{oneshot, Mutex};

use super::types::{AcpError, IncomingMessage, RequestId};

pub(crate) type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, AcpError>>>>>;

/// Shared request-response matching layer used by all transport implementations.
///
/// Maintains a map of pending request IDs → oneshot senders. The pump loop in each
/// transport calls [`dispatch`] to check incoming Responses against this map;
/// matched responses are routed to the correct caller via the oneshot channel.
#[derive(Clone)]
pub(crate) struct RequestRouter {
    pending: PendingMap,
    next_id: Arc<AtomicI64>,
}

impl RequestRouter {
    /// Creates a new router with its own pending map and ID counter.
    /// Use this for standalone transports like `StdioTransport`.
    pub(crate) fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicI64::new(1)),
        }
    }

    /// Creates a router sharing another router's pending map and ID counter.
    /// Use this for paired transports like `MpscClientTransport` + `MpscServerTransport`.
    pub(crate) fn new_shared(pending: PendingMap, next_id: Arc<AtomicI64>) -> Self {
        Self { pending, next_id }
    }

    /// Allocates a new request ID, inserts a oneshot sender into the pending map,
    /// and returns the (id_num, receiver) pair. The caller sends the request message
    /// and then `.await`s the receiver for the response.
    pub(crate) async fn register(&self) -> (i64, oneshot::Receiver<Result<Value, AcpError>>) {
        let id_num = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id_num, tx);
        (id_num, rx)
    }

    /// Dispatches an incoming message. If it's a Response whose id matches a pending
    /// request, the oneshot sender is removed from the map and the result is sent.
    /// Returns `true` if the message was consumed (matched response), `false` if it
    /// should be forwarded to the caller as an unmatched `IncomingMessage`.
    ///
    /// # String IDs
    /// `RequestId::String` variants are never matched — all pending keys are `i64`.
    /// They fall through to the unmatched-forward path.
    pub(crate) async fn dispatch(&self, msg: &IncomingMessage) -> bool {
        match msg {
            IncomingMessage::Response { id, result } => {
                if let RequestId::Number(n) = id {
                    if let Some(tx) = self.pending.lock().await.remove(n) {
                        let _ = tx.send(result.clone());
                        return true; // consumed — caller should NOT forward
                    }
                }
                false // unmatched — caller should forward
            }
            _ => false, // Requests and Notifications are never consumed by the router
        }
    }
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
