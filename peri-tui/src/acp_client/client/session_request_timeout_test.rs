use super::{AcpTuiClient, json};

use peri_acp::transport::types::IncomingMessage;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn session_request_default_timeout_is_ten_seconds() {
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().expect("session request default timeout is ten seconds: the stable_session_identity fixture operation uses valid inputs");
    let task = tokio::spawn(async move {
        client
            .send_session_request(&identity, "cron/list", json!({"sessionId":"session-a"}))
            .await
    });
    let Some(IncomingMessage::Request { .. }) = server.recv().await else {
        panic!("ordinary session request was not sent")
    };

    tokio::time::advance(Duration::from_secs(9)).await;
    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "ordinary RPC expired before ten seconds"
    );
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
    let error = tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("ordinary RPC must expire at its ten-second deadline")
        .expect("session request default timeout is ten seconds: fixture task and request must complete successfully")
        .expect_err("session request default timeout is ten seconds: the invalid fixture must be rejected");
    assert_eq!(error.code, -32603);
    assert_eq!(error.data.expect("session request default timeout is ten seconds: the fixture establishes error.data before this assertion")["kind"], "request_timeout");
}

#[tokio::test(start_paused = true)]
async fn session_request_custom_timeout_bounds_operation_gate_wait() {
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().expect("session request custom timeout bounds operation gate wait: the stable_session_identity fixture operation uses valid inputs");
    let gate = client.lifecycle.operation_gate();
    let operation = gate.lock().await;
    let requester = client.clone();
    let task = tokio::spawn(async move {
        requester
            .send_session_request_with_timeout(
                &identity,
                "peri/mcp/control",
                json!({"sessionId":"session-a","serverName":"docs","action":"stop"}),
                Duration::from_secs(60),
            )
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "custom deadline was truncated");
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
    let error = tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("gate admission must not outlive the sixty-second deadline")
        .expect("session request custom timeout bounds operation gate wait: fixture task and request must complete successfully")
        .expect_err("session request custom timeout bounds operation gate wait: the invalid fixture must be rejected");
    assert_eq!(error.data.expect("session request custom timeout bounds operation gate wait: the fixture establishes error.data before this assertion")["kind"], "request_timeout");
    drop(operation);
    assert!(
        tokio::time::timeout(Duration::from_millis(1), server.recv())
            .await
            .is_err(),
        "expired admission must not send a request after the gate is released"
    );
}

#[tokio::test(start_paused = true)]
async fn session_request_custom_timeout_is_shared_by_admission_and_wire_wait() {
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().expect("session request custom timeout is shared by admission and wire wait: the stable_session_identity fixture operation uses valid inputs");
    let gate = client.lifecycle.operation_gate();
    let operation = gate.lock().await;
    let requester = client.clone();
    let task = tokio::spawn(async move {
        requester
            .send_session_request_with_timeout(
                &identity,
                "peri/mcp/control",
                json!({"sessionId":"session-a","serverName":"docs","action":"restart"}),
                Duration::from_secs(60),
            )
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(59)).await;
    drop(operation);
    let Some(IncomingMessage::Request { .. }) = server.recv().await else {
        panic!("request was not admitted after the gate was released")
    };
    assert!(
        gate.try_lock().is_ok(),
        "wire response wait must release the operation gate"
    );
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
    let error = tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("wire wait must share the original admission deadline")
        .expect("session request custom timeout is shared by admission and wire wait: fixture task and request must complete successfully")
        .expect_err("session request custom timeout is shared by admission and wire wait: the invalid fixture must be rejected");
    assert_eq!(error.data.expect("session request custom timeout is shared by admission and wire wait: the fixture establishes error.data before this assertion")["kind"], "request_timeout");
}

use peri_acp::transport::AcpTransport;
