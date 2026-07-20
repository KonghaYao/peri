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
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_register_sequential_ids() {
        let router = RequestRouter::new();
        let (id1, _rx1) = router.register().await;
        let (id2, _rx2) = router.register().await;
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[tokio::test]
    async fn test_dispatch_matched_response() {
        let router = RequestRouter::new();
        let (id, rx) = router.register().await;
        let msg = IncomingMessage::Response {
            id: RequestId::Number(id),
            result: Ok(json!("hello")),
        };
        assert!(router.dispatch(&msg).await);
        assert_eq!(rx.await.unwrap().unwrap(), json!("hello"));
    }

    #[tokio::test]
    async fn test_dispatch_unmatched_response() {
        let router = RequestRouter::new();
        let msg = IncomingMessage::Response {
            id: RequestId::Number(999),
            result: Ok(json!("orphan")),
        };
        assert!(!router.dispatch(&msg).await);
    }

    #[tokio::test]
    async fn test_dispatch_request_not_consumed() {
        let router = RequestRouter::new();
        let msg = IncomingMessage::Request {
            id: RequestId::Number(1),
            method: "test".into(),
            params: json!({}),
        };
        assert!(!router.dispatch(&msg).await);
    }

    #[tokio::test]
    async fn test_dispatch_notification_not_consumed() {
        let router = RequestRouter::new();
        let msg = IncomingMessage::Notification {
            method: "test".into(),
            params: json!({}),
        };
        assert!(!router.dispatch(&msg).await);
    }

    #[tokio::test]
    async fn test_shared_router_sees_both_ids() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicI64::new(1));
        let r1 = RequestRouter::new_shared(pending.clone(), next_id.clone());
        let r2 = RequestRouter::new_shared(pending.clone(), next_id.clone());
        let (id1, rx1) = r1.register().await;
        let (id2, _rx2) = r2.register().await;
        // IDs should interleave across shared counter
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        // r2's pending map can receive r1's response
        let msg = IncomingMessage::Response {
            id: RequestId::Number(id1),
            result: Ok(json!("shared")),
        };
        assert!(r2.dispatch(&msg).await);
        assert_eq!(rx1.await.unwrap().unwrap(), json!("shared"));
    }
}
