use super::*;
use crate::acp_client::AcpNotification;
use peri_acp::event::AcpEvent;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};

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

#[tokio::test]
async fn managed_cancel_preserves_matching_terminal_but_rejects_duplicate() {
    let (transport, server) = mpsc_transport_pair();
    let (client, notifications, mut rx) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session", false);
    client
        .user_input_queue
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(
        client
            .lifecycle
            .bind_user_input_generation("session", 1, "generation")
    );
    client.spawn_pump(notifications);
    server
        .send_notification(
            "peri/agent_event",
            json!({"sessionId":"session", "event": AcpEvent::UserInputRunStarted {
                generation:"generation".into(), request_id:"run".into(),
            }}),
        )
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(AcpNotification::AgentEvent { .. })
    ));
    client.cancel().await.unwrap();
    assert!(
        matches!(server.recv().await, Some(IncomingMessage::Notification { method, params })
        if method == "session/cancel" && params["generation"] == "generation" && params["requestId"] == "run")
    );
    let done = json!({"sessionId":"session", "requestId":"run", "stopReason":"cancelled"});
    server
        .send_notification("peri/agent_event_done", done.clone())
        .await
        .unwrap();
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
    if terminal.is_err() {
        client.close();
    }
    assert!(
        matches!(terminal.unwrap(), Some(AcpNotification::AgentDone { stop_reason, request_id, .. })
        if stop_reason == "cancelled" && request_id.as_deref() == Some("run"))
    );
    server
        .send_notification("peri/agent_event_done", done)
        .await
        .unwrap();
    server
        .send_notification(
            "session/update",
            json!({"sessionId":"session", "update":{}}),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .unwrap(),
            Some(AcpNotification::SessionUpdate { .. })
        ),
        "duplicate terminal must be dropped before the next notification"
    );
    client.close();
}
