//! Stdio-based ACP transport for IDE integration.
//!
//! Reads JSON-RPC messages from stdin (one per line) and writes to stdout.
//! Background pump task dispatches Response messages to the pending request map,
//! forwards Requests/Notifications to the incoming channel.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{mpsc, Mutex},
};

use super::{
    router::RequestRouter,
    types::{AcpError, IncomingMessage, RequestId},
    AcpTransport,
};

/// JSON-RPC 2.0 envelope for (de)serialization over stdio.
#[derive(serde::Serialize, serde::Deserialize)]
struct JsonRpcEnvelope {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<AcpError>,
}

/// Stdio-based ACP transport.
///
/// Communicates with an external client (IDE) over stdin/stdout using
/// newline-delimited JSON-RPC 2.0 messages. A background pump task reads
/// stdin lines, dispatches responses to pending requests, and forwards
/// requests/notifications to the `recv()` channel.
pub struct StdioTransport {
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<IncomingMessage>>,
    router: RequestRouter,
    writer: Arc<Mutex<BufWriter<tokio::io::Stdout>>>,
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl StdioTransport {
    /// Create a new stdio transport. Must be called within a tokio runtime.
    pub fn new() -> Self {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let router = RequestRouter::new();
        let pump_router = router.clone();

        // Background pump: read stdin → dispatch responses / forward messages
        tokio::spawn(async move {
            let stdin = BufReader::new(tokio::io::stdin());
            let mut lines = stdin.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }

                let mut envelope: JsonRpcEnvelope = match serde_json::from_str(&line) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to parse JSON-RPC from stdin");
                        continue;
                    }
                };

                let has_method = envelope.method.is_some();
                let result_val = envelope.result.take();
                let error_val = envelope.error.take();

                match (envelope.id, has_method) {
                    // Response to a server-initiated request (has id, no method)
                    (Some(id), false) => {
                        let req_id = value_to_request_id(&id);
                        let result = if let Some(error) = error_val {
                            Err(error)
                        } else {
                            Ok(result_val.unwrap_or(Value::Null))
                        };
                        let msg = IncomingMessage::Response { id: req_id, result };
                        if !pump_router.dispatch(&msg).await {
                            let _ = incoming_tx.send(msg);
                        }
                    }
                    // Request (has id + method)
                    (Some(id), true) => {
                        let method = envelope.method.unwrap();
                        let req_id = value_to_request_id(&id);
                        let _ = incoming_tx.send(IncomingMessage::Request {
                            id: req_id,
                            method,
                            params: envelope.params.unwrap_or(Value::Null),
                        });
                    }
                    // Notification (no id, has method)
                    (None, true) => {
                        let method = envelope.method.unwrap();
                        let _ = incoming_tx.send(IncomingMessage::Notification {
                            method,
                            params: envelope.params.unwrap_or(Value::Null),
                        });
                    }
                    _ => {
                        tracing::warn!("Unhandled JSON-RPC message structure, ignoring");
                    }
                }
            }

            tracing::info!("Stdio transport: stdin closed");
        });

        Self {
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            router,
            writer: Arc::new(Mutex::new(BufWriter::new(tokio::io::stdout()))),
        }
    }
}

#[async_trait]
impl AcpTransport for StdioTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let (id_num, response_rx) = self.router.register().await;

        let envelope = JsonRpcEnvelope {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(id_num.into())),
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        };
        write_envelope(&self.writer, &envelope).await?;

        response_rx
            .await
            .map_err(|_| AcpError::new(-32603, "Request cancelled (client disconnected)"))?
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let envelope = JsonRpcEnvelope {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        };
        write_envelope(&self.writer, &envelope).await
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        let id_val = request_id_to_value(&id);
        match result {
            Ok(value) => {
                let envelope = JsonRpcEnvelope {
                    jsonrpc: "2.0".to_string(),
                    id: Some(id_val),
                    method: None,
                    params: None,
                    result: Some(value),
                    error: None,
                };
                write_envelope(&self.writer, &envelope).await
            }
            Err(error) => {
                let envelope = JsonRpcEnvelope {
                    jsonrpc: "2.0".to_string(),
                    id: Some(id_val),
                    method: None,
                    params: None,
                    result: None,
                    error: Some(error),
                };
                write_envelope(&self.writer, &envelope).await
            }
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

async fn write_envelope(
    writer: &Arc<Mutex<BufWriter<tokio::io::Stdout>>>,
    envelope: &JsonRpcEnvelope,
) -> Result<(), AcpError> {
    let mut line = serde_json::to_string(envelope)
        .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))?;
    line.push('\n');
    let mut guard = writer.lock().await;
    guard
        .write_all(line.as_bytes())
        .await
        .map_err(|e| AcpError::new(-32603, format!("Write failed: {e}")))?;
    guard
        .flush()
        .await
        .map_err(|e| AcpError::new(-32603, format!("Flush failed: {e}")))?;
    Ok(())
}

fn value_to_request_id(v: &Value) -> RequestId {
    match v {
        Value::String(s) => RequestId::String(s.clone()),
        Value::Number(n) => RequestId::Number(n.as_i64().unwrap_or(0)),
        _ => RequestId::Number(0),
    }
}

fn request_id_to_value(id: &RequestId) -> Value {
    match id {
        RequestId::String(s) => Value::String(s.clone()),
        RequestId::Number(n) => Value::Number((*n).into()),
    }
}

#[cfg(test)]
#[path = "stdio_test.rs"]
mod tests;
