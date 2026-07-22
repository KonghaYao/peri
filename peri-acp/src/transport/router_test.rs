//! Tests for router

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
