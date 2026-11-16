use std::sync::Arc;

use peri_acp::transport::AcpTransport;
use serde_json::json;

use super::AcpTuiClient;
use super::CancellationToken;
use super::HitlResponseAction;
use super::mpsc;

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client_transport, _) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(client_transport);
    let (_tx, rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = super::spawn_hitl_response_consumer(client, rx, shutdown.clone());
    shutdown.cancel();
    handle
        .await
        .expect("shutdown exits loop: fixture task and request must complete successfully");
}

async fn run_hitl_action(approve: bool) -> serde_json::Value {
    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s1", true);
    client.spawn_pump(notification_tx);
    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = super::spawn_hitl_response_consumer(client, rx, shutdown.clone());
    let server = Arc::new(server_transport);
    let request_server = server.clone();
    let request = tokio::spawn(async move {
        request_server
            .send_request("session/request_permission", json!({"sessionId":"s1"}))
            .await
            .expect("run hitl action: fixture task and request must complete successfully")
    });
    let crate::acp_client::AcpNotification::RequestPermission {
        owner,
        request_id_json,
        ..
    } = notification_rx
        .recv()
        .await
        .expect("run hitl action: fixture task and request must complete successfully")
    else {
        panic!()
    };
    let action = if approve {
        HitlResponseAction::Approve {
            owner,
            request_id_str: request_id_json,
        }
    } else {
        HitlResponseAction::Reject {
            owner,
            request_id_str: request_id_json,
        }
    };
    tx.send(action)
        .expect("run hitl action: fixture channel must deliver its expected message");
    let response = request
        .await
        .expect("run hitl action: fixture task and request must complete successfully");
    let terminal = notification_rx
        .recv()
        .await
        .expect("run hitl action: fixture task and request must complete successfully");
    assert!(matches!(
        terminal,
        crate::acp_client::AcpNotification::InteractionTerminal { .. }
    ));
    shutdown.cancel();
    handle
        .await
        .expect("run hitl action: fixture task and request must complete successfully");
    response
}

#[tokio::test]
async fn test_hitl_response_approve_sends_selected_allow_once() {
    let response = run_hitl_action(true).await;
    assert_eq!(response["outcome"]["outcome"], "selected");
    assert_eq!(response["outcome"]["optionId"], "allow_once");
}

#[tokio::test]
async fn test_hitl_response_reject_sends_cancelled() {
    let response = run_hitl_action(false).await;
    assert_eq!(response["outcome"]["outcome"], "cancelled");
}

#[test]
fn test_hitl_response_action_variants_carry_owner() {
    let owner = crate::acp_client::InteractionOwner::default();
    let approve = HitlResponseAction::Approve {
        owner: owner.clone(),
        request_id_str: "\"abc\"".into(),
    };
    let HitlResponseAction::Approve { owner: actual, .. } = approve else {
        panic!()
    };
    assert_eq!(actual, owner);
}
