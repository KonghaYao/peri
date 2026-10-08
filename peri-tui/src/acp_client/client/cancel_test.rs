use super::*;
use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};

#[tokio::test]
async fn cancel_sends_plain_notification_without_recovery_protocol() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session", false);
    client.cancel().await.unwrap();
    let IncomingMessage::Notification { method, params } = server.recv().await.unwrap() else {
        panic!("expected cancellation notification");
    };
    assert_eq!(method, "session/cancel");
    assert_eq!(params["sessionId"], "session");
    assert!(params.get("commandId").is_none());
    client.close();
}

#[tokio::test]
async fn cancel_without_session_is_an_error() {
    let (transport, _server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    let error = client.cancel().await.unwrap_err();
    assert_eq!(error.code, -32603);
    client.close();
}
