use super::*;

#[tokio::test]
async fn test_aborted_prompt_lease_settles_registered_reverse() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(notification_tx);
    let prompt_client = client.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .prompt(
                &peri_acp_types::messages::MessageContent::text("hi"),
                Some("p1".into()),
            )
            .await
    });
    let server = Arc::new(server_transport);
    let IncomingMessage::Request { method, .. } = server.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(method, "session/prompt");
    let reverse_server = server.clone();
    let reverse = tokio::spawn(async move {
        reverse_server
            .send_request("session/request_permission", json!({"sessionId":"s1"}))
            .await
            .unwrap()
    });
    assert!(matches!(
        notification_rx.recv().await.unwrap(),
        AcpNotification::RequestPermission { .. }
    ));
    prompt.abort();
    let response = reverse.await.unwrap();
    assert_cancel_response("session/request_permission", response);
}

#[tokio::test]
async fn test_unidentified_or_stale_turn_terminal_preserves_prompt_b() {
    let lifecycle = lifecycle(Some("s1"), false);
    let lease_a = lifecycle.open_prompt(Some("A".into())).unwrap();
    drop(lease_a);
    let _lease_b = lifecycle.open_prompt(Some("B".into())).unwrap();
    let b = match lifecycle.register_reverse(
        ReverseInteractionKind::Permission,
        RequestId::Number(9),
        Some("s1"),
        json!({}),
    ) {
        RegisterDecision::Forward(request) => request,
        _ => panic!(),
    };
    assert!(
        lifecycle
            .close_prompt_by_wire_identity("s1", None)
            .is_empty()
    );
    assert!(
        lifecycle
            .close_prompt_by_wire_identity("s1", Some("A"))
            .is_empty()
    );
    assert!(lifecycle.is_pending_owner(&b.owner));
}

#[tokio::test]
async fn test_aborted_transition_guard_queues_unexecuted_claims_once() {
    let lifecycle = lifecycle(Some("s1"), true);
    for id in [1, 2] {
        assert!(matches!(
            lifecycle.register_reverse(
                ReverseInteractionKind::Permission,
                RequestId::Number(id),
                Some("s1"),
                json!({}),
            ),
            RegisterDecision::Forward(_)
        ));
    }
    let (settlement_tx, mut settlement_rx) = tokio::sync::mpsc::unbounded_channel();
    lifecycle.install_drop_settlement_sender(settlement_tx);
    let start = lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    let mut batch = lifecycle.arm_claimed_batch(start.claims);
    let in_flight = batch.next_claim().unwrap();
    drop(in_flight);
    drop(batch);
    assert_eq!(
        settlement_rx.try_recv().unwrap().request_id,
        RequestId::Number(1)
    );
    assert_eq!(
        settlement_rx.try_recv().unwrap().request_id,
        RequestId::Number(2)
    );
    assert!(matches!(settlement_rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn test_aborted_real_transition_future_fails_closed_and_can_recover() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.new_session("/tmp", None).await });
    let (_abandoned_id, _) = receive_lifecycle_request(&server_transport, "session/new").await;

    // The future is armed and suspended on its real MPSC lifecycle response.
    // Dropping it must synchronously run TransitionLease::drop -> NoSession.
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(client.current_session_id(), None);
    assert!(!client.has_session());

    let retry_client = client.clone();
    let retry = tokio::spawn(async move { retry_client.ensure_session("/tmp", None).await });
    let (retry_id, _) = receive_lifecycle_request(&server_transport, "session/new").await;
    server_transport
        .send_response(retry_id, Ok(json!({"sessionId":"recovered"})))
        .await
        .unwrap();
    assert_eq!(retry.await.unwrap().unwrap(), "recovered");
}
