use super::*;
use crate::acp_client::interaction_lifecycle::{
    ClaimCause, InteractionLifecycle, RegisterDecision, ReverseInteractionKind, TransitionKind,
};
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn test_expiry_for_claim_cause_is_stable() {
    assert_eq!(
        expiry_for_cause(ClaimCause::LifecycleDrain),
        InteractionExpiryReason::LifecycleDrain
    );
    assert_eq!(
        expiry_for_cause(ClaimCause::TurnTerminal),
        InteractionExpiryReason::TurnTerminal
    );
}

#[tokio::test]
async fn test_drop_settlement_worker_settles_real_wire_and_expires_owner() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let transport = Arc::new(client_transport);
    let server = Arc::new(server_transport);
    let request_server = server.clone();
    let reverse = tokio::spawn(async move {
        request_server
            .send_request("session/request_permission", json!({"sessionId":"s1"}))
            .await
            .unwrap()
    });
    let IncomingMessage::Request { id, .. } = transport.recv().await.unwrap() else {
        panic!("expected real reverse wire request")
    };

    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("s1", true);
    let registered = match lifecycle.register_reverse(
        ReverseInteractionKind::Permission,
        id,
        Some("s1"),
        json!({}),
    ) {
        RegisterDecision::Forward(registered) => registered,
        _ => panic!("owner must register before transition"),
    };
    let (settlement_tx, settlement_rx) = tokio::sync::mpsc::unbounded_channel();
    lifecycle.install_drop_settlement_sender(settlement_tx);
    let (notification_tx, mut notification_rx) = tokio::sync::mpsc::unbounded_channel();
    let weak_notification = Arc::new(Mutex::new(Some(notification_tx.downgrade())));
    spawn_settlement_worker(Arc::downgrade(&transport), settlement_rx, weak_notification);

    let start = lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    let transition = lifecycle.arm_transition(start.generation);
    let batch = lifecycle.arm_claimed_batch(start.claims);
    drop(batch);
    drop(transition);

    assert_eq!(reverse.await.unwrap(), permission_cancelled_response());
    assert_eq!(lifecycle.current_session_id(), None);
    let AcpNotification::InteractionTerminal { owner, outcome } =
        notification_rx.recv().await.unwrap()
    else {
        panic!("worker must publish owner-aware expiry")
    };
    assert_eq!(owner, registered.owner);
    assert_eq!(
        outcome,
        InteractionUiOutcome::Expired {
            reason: InteractionExpiryReason::LifecycleDrain,
        }
    );
    assert!(notification_rx.try_recv().is_err());
}
