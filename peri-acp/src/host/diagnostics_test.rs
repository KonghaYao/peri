use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tracing::instrument::WithSubscriber;

use super::*;
use crate::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        self.subscriber_at(tracing::Level::INFO)
    }

    fn subscriber_at(&self, level: tracing::Level) -> impl tracing::Subscriber + Send + Sync {
        let buffer = self.clone();
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(level)
            .with_writer(move || buffer.clone())
            .finish()
    }
}

#[tokio::test]
async fn error_response_logs_context_without_payload_and_preserves_wire_error() {
    for (code, level) in [
        (-32010, "WARN"),
        (-32603, "ERROR"),
        (-32602, "WARN"),
        (-32601, "WARN"),
    ] {
        let (client, server) = mpsc_transport_pair();
        let request = tokio::spawn(async move {
            client
                .send_request("session/load", json!({"sessionId": "session-42"}))
                .await
        });
        let Some(IncomingMessage::Request { id, method, params }) = server.recv().await else {
            panic!("expected request");
        };
        let rpc_id = id.to_string();
        let mut params = params;
        params["token"] = json!("private-request-token");
        let buffer = LogBuffer::default();
        ResponseDiagnostics::new(id, &method, &params)
            .send(
                &server,
                Err(AcpError::new(code, "restore incomplete")
                    .with_data(json!({"secret": "private-error-data"}))),
            )
            .with_subscriber(buffer.subscriber_at(if code == -32603 {
                tracing::Level::ERROR
            } else {
                tracing::Level::WARN
            }))
            .await
            .unwrap();
        let error = request.await.unwrap().unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.message, "restore incomplete");
        assert_eq!(error.data, Some(json!({"secret": "private-error-data"})));
        let logs = buffer.text();
        for expected in [
            level,
            "session/load",
            "session-42",
            "restore incomplete",
            &format!("rpc_id={rpc_id}"),
            &format!("code={code}"),
        ] {
            assert!(logs.contains(expected), "missing {expected}: {logs}");
        }
        assert!(!logs.contains("private-request-token"));
        assert!(!logs.contains("private-error-data"));
    }
}

#[tokio::test]
async fn successful_and_cancelled_responses_are_not_warning_logs() {
    for result in [
        Ok(json!({"ok": true})),
        Err(AcpError::new(-32800, "request cancelled")),
    ] {
        let (_client, server) = mpsc_transport_pair();
        let buffer = LogBuffer::default();
        ResponseDiagnostics::new(RequestId::Number(1), "session/prompt", &json!({}))
            .send(&server, result)
            .with_subscriber(buffer.subscriber())
            .await
            .unwrap();
        assert!(buffer.text().is_empty(), "{}", buffer.text());
    }
}

#[tokio::test]
async fn delivery_failure_is_logged_even_for_successful_response() {
    let (client, server) = mpsc_transport_pair();
    client.close();
    let buffer = LogBuffer::default();
    let error = ResponseDiagnostics::new(
        RequestId::String("rpc-42".into()),
        "peri/mcp/invoke",
        &json!({"ownerSessionId": "owner-42"}),
    )
    .send(&server, Ok(Value::Null))
    .with_subscriber(buffer.subscriber())
    .await
    .unwrap_err();
    assert_eq!(error.code, -32603);
    let logs = buffer.text();
    for expected in [
        "WARN",
        "peri/mcp/invoke",
        "rpc-42",
        "owner-42",
        "ACP response delivery failed",
        "Transport closed",
    ] {
        assert!(logs.contains(expected), "missing {expected}: {logs}");
    }
}

#[test]
fn legacy_session_id_is_retained_in_diagnostics() {
    let diagnostics = ResponseDiagnostics::new(
        RequestId::Number(1),
        "session/load",
        &json!({"session_id": "legacy-session"}),
    );
    assert_eq!(diagnostics.session_id.as_deref(), Some("legacy-session"));
}

#[tokio::test]
async fn session_update_delivery_failure_logs_metadata_without_payload() {
    let (client, server) = mpsc_transport_pair();
    client.close();
    let buffer = LogBuffer::default();
    send_session_update(
        &server,
        "session-42",
        "config_options_update",
        json!({"token": "private-notification-data"}),
    )
    .with_subscriber(buffer.subscriber())
    .await;
    let logs = buffer.text();
    for expected in [
        "ERROR",
        "session/update",
        "session-42",
        "config_options_update",
        "ACP notification delivery failed",
    ] {
        assert!(logs.contains(expected), "missing {expected}: {logs}");
    }
    assert!(!logs.contains("private-notification-data"));
}

#[tokio::test]
async fn successful_session_update_is_unchanged_and_not_logged() {
    let (_client, server) = mpsc_transport_pair();
    let buffer = LogBuffer::default();
    send_session_update(&server, "session-42", "session_info_update", Value::Null)
        .with_subscriber(buffer.subscriber())
        .await;
    assert!(buffer.text().is_empty(), "{}", buffer.text());
}
