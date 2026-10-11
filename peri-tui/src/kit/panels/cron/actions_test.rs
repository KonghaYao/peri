use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::mpsc_transport_pair,
    types::{AcpError, IncomingMessage},
};

#[tokio::test]
async fn mutations_send_session_and_job_identity_and_surface_server_failure() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    let server_task = tokio::spawn(async move {
        for expected in ["cron/toggle", "cron/remove"] {
            let Some(IncomingMessage::Request { id, method, params }) = server.recv().await else {
                panic!("expected cron wire request");
            };
            assert_eq!(method, expected);
            assert_eq!(params, json!({"sessionId":"session-a","id":"job-a"}));
            let response = if expected == "cron/toggle" {
                Ok(json!({"id":"job-a","success":true}))
            } else {
                Err(AcpError::new(-32602, "job not found"))
            };
            server.send_response(id, response).await.unwrap();
        }
    });
    request(&client, &identity, "cron/toggle", "job-a")
        .await
        .unwrap();
    assert_eq!(
        request(&client, &identity, "cron/remove", "job-a")
            .await
            .unwrap_err()
            .code,
        -32602
    );
    server_task.await.unwrap();
}

#[tokio::test]
async fn stale_generation_is_rejected_before_sending_even_for_same_session() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let mut identity = client.stable_session_identity().unwrap();
    identity.1 += 1;
    assert_eq!(
        request(&client, &identity, "cron/remove", "job-a")
            .await
            .unwrap_err()
            .code,
        -32602
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), server.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_success_cannot_be_reported_as_mutation_success() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    tokio::spawn(async move {
        let Some(IncomingMessage::Request { id, .. }) = server.recv().await else {
            panic!("request")
        };
        server
            .send_response(id, Ok(json!({"id":"other","success":true})))
            .await
            .unwrap();
    });
    assert_eq!(
        request(&client, &identity, "cron/toggle", "job-a")
            .await
            .unwrap_err()
            .code,
        -32603
    );
}
