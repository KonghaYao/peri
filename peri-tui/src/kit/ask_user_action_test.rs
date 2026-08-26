use std::sync::Arc;

use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair};
use serde_json::json;

use super::*;

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client_transport, _) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(client_transport);
    let (_tx, rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_ask_user_consumer(client, rx, shutdown.clone());
    shutdown.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn test_ask_user_submit_sends_full_answers_once() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s1", true);
    client.spawn_pump(notification_tx);
    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_ask_user_consumer(client, rx, shutdown.clone());
    let server = Arc::new(server_transport);
    let request_server = server.clone();
    let request = tokio::spawn(async move {
        request_server
            .send_request(
                "elicitation/create",
                json!({"sessionId":"s1","requestedSchema":{"type":"object","properties":{}}}),
            )
            .await
            .unwrap()
    });
    let crate::acp_client::AcpNotification::Elicitation {
        owner,
        request_id_json,
        ..
    } = notification_rx.recv().await.unwrap()
    else {
        panic!()
    };
    tx.send(AskUserResponseAction::Submit {
        owner,
        request_id_str: request_id_json,
        answers: json!({"q1":"Fast","q2":"Complete"}),
    })
    .unwrap();
    let response = request.await.unwrap();
    assert_eq!(response["action"], "accept");
    assert_eq!(response["content"], json!({"q1":"Fast","q2":"Complete"}));
    assert!(matches!(
        notification_rx.recv().await.unwrap(),
        crate::acp_client::AcpNotification::InteractionTerminal { .. }
    ));
    shutdown.cancel();
    handle.await.unwrap();
}

#[test]
fn test_ask_user_response_action_variants_carry_owner() {
    let owner = crate::acp_client::InteractionOwner::default();
    let submit = AskUserResponseAction::Submit {
        owner: owner.clone(),
        request_id_str: "\"abc\"".into(),
        answers: json!({"q1":"yes"}),
    };
    let AskUserResponseAction::Submit {
        owner: actual,
        answers,
        ..
    } = submit
    else {
        panic!()
    };
    assert_eq!(actual, owner);
    assert_eq!(answers["q1"], "yes");
}
