use super::*;
use crate::acp_client::interaction_lifecycle::TransitionKind;
use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};

#[tokio::test]
async fn session_request_does_not_hold_operation_gate_while_waiting_for_wire_response() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    let requester = client.clone();
    let task = tokio::spawn(async move {
        requester
            .send_session_request(&identity, "cron/list", json!({"sessionId":"session-a"}))
            .await
    });
    let Some(IncomingMessage::Request { id, .. }) = server.recv().await else {
        panic!("request")
    };
    let gate = client.lifecycle.operation_gate();
    let operation = tokio::time::timeout(std::time::Duration::from_millis(100), gate.lock())
        .await
        .expect("network wait cannot own operation gate");
    drop(operation);
    server
        .send_response(id, Ok(json!({"jobs":[]})))
        .await
        .unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn reloading_same_session_retires_previous_request_identity() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let old = client.stable_session_identity().unwrap();
    let transition = client
        .lifecycle
        .begin_transition(TransitionKind::Load, Some("session-a".into()))
        .unwrap();
    client
        .lifecycle
        .commit_stable(transition.generation, "session-a".into());
    assert_ne!(client.stable_session_identity().unwrap(), old);
    assert!(
        client
            .send_session_request(
                &old,
                "cron/remove",
                json!({"sessionId":"session-a","id":"job"})
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), server.recv())
            .await
            .is_err()
    );
}
