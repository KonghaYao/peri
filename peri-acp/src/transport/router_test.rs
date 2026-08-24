//! Tests for router.

use super::*;
use futures::{pin_mut, task::noop_waker_ref};
use serde_json::json;
use std::{
    future::Future,
    task::{Context, Poll},
};

fn assert_transport_closed(error: AcpError) {
    assert_eq!(error.code, -32603);
    assert_eq!(error.message, "Transport closed");
    assert!(error.data.is_none());
}

#[tokio::test]
async fn test_register_allocates_sequential_ids_across_clones() {
    let router = RequestRouter::new();
    let clone = router.clone();
    let pending1 = router.register().expect("first request should register");
    let pending2 = clone.register().expect("second request should register");
    assert_eq!(pending1.id(), 1);
    assert_eq!(pending2.id(), 2);
}

#[tokio::test]
async fn test_dispatch_matched_response() {
    let router = RequestRouter::new();
    let pending = router.register().expect("request should register");
    let msg = IncomingMessage::Response {
        id: RequestId::Number(pending.id()),
        result: Ok(json!("hello")),
    };
    assert!(router.dispatch(&msg));
    assert_eq!(pending.wait().await.unwrap(), json!("hello"));
}

#[test]
fn test_dispatch_unmatched_response() {
    let router = RequestRouter::new();
    let msg = IncomingMessage::Response {
        id: RequestId::Number(999),
        result: Ok(json!("orphan")),
    };
    assert!(!router.dispatch(&msg));
}

#[test]
fn test_dispatch_non_response_not_consumed() {
    let router = RequestRouter::new();
    let request = IncomingMessage::Request {
        id: RequestId::Number(1),
        method: "test".into(),
        params: json!({}),
    };
    let notification = IncomingMessage::Notification {
        method: "test".into(),
        params: json!({}),
    };
    assert!(!router.dispatch(&request));
    assert!(!router.dispatch(&notification));
}

/// [回归测试] transport 终止必须同时结算所有已登记请求，重复 close 不得改变结果。
#[tokio::test]
async fn test_close_settles_all_pending_and_is_idempotent() {
    let router = RequestRouter::new();
    let pending1 = router.register().expect("first request should register");
    let pending2 = router.register().expect("second request should register");
    router.close();
    router.close();
    assert_transport_closed(pending1.wait().await.unwrap_err());
    assert_transport_closed(pending2.wait().await.unwrap_err());
}

#[test]
fn test_register_after_close_fails_immediately() {
    let router = RequestRouter::new();
    router.close();
    assert_transport_closed(router.register().unwrap_err());
}

#[tokio::test]
async fn test_response_before_close_wins() {
    let router = RequestRouter::new();
    let pending = router.register().expect("request should register");
    let response = IncomingMessage::Response {
        id: RequestId::Number(pending.id()),
        result: Ok(json!("response won")),
    };
    assert!(router.dispatch(&response));
    router.close();
    assert_eq!(pending.wait().await.unwrap(), json!("response won"));
}

#[tokio::test]
async fn test_close_before_response_wins_and_late_response_is_unmatched() {
    let router = RequestRouter::new();
    let pending = router.register().expect("request should register");
    let id = pending.id();
    router.close();
    let response = IncomingMessage::Response {
        id: RequestId::Number(id),
        result: Ok(json!("too late")),
    };
    assert!(!router.dispatch(&response));
    assert_transport_closed(pending.wait().await.unwrap_err());
}

#[test]
fn test_dropped_pending_request_unregisters_synchronously() {
    let router = RequestRouter::new();
    let pending = router.register().expect("request should register");
    let id = pending.id();
    assert!(router.state.lock().pending.contains_key(&id));
    drop(pending);
    assert!(!router.state.lock().pending.contains_key(&id));
}

#[test]
fn test_stale_handle_cannot_unregister_reused_id() {
    let router = RequestRouter::new();
    let stale = router.register().expect("stale request should register");
    let reused_id = stale.id();
    {
        let mut state = router.state.lock();
        state.pending.remove(&reused_id);
        state.next_id = reused_id;
    }
    let current = router.register().expect("reused request should register");
    assert_eq!(current.id(), reused_id);
    drop(stale);
    assert!(router.state.lock().pending.contains_key(&reused_id));
}

#[test]
fn test_id_allocation_wraps_to_positive_domain_and_skips_occupied_ids() {
    let router = RequestRouter::new();
    router.state.lock().next_id = i64::MAX;
    let maximum = router.register().expect("maximum id should register");
    let one = router.register().expect("allocator should wrap to one");
    let two = router
        .register()
        .expect("allocator should skip occupied one");
    assert_eq!(maximum.id(), i64::MAX);
    assert_eq!(one.id(), 1);
    assert_eq!(two.id(), 2);
}

#[tokio::test]
async fn test_wait_closed_completes_when_close_precedes_wait() {
    let router = RequestRouter::new();
    router.close();
    router.wait_closed().await;
}

#[test]
fn test_wait_closed_completes_when_wait_precedes_close() {
    let router = RequestRouter::new();
    let waiter = router.wait_closed();
    pin_mut!(waiter);
    let mut context = Context::from_waker(noop_waker_ref());
    assert!(
        matches!(waiter.as_mut().poll(&mut context), Poll::Pending),
        "wait_closed must be pending before close"
    );
    router.close();
    assert!(
        matches!(waiter.as_mut().poll(&mut context), Poll::Ready(())),
        "the same pre-polled waiter must observe close"
    );
}
