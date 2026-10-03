//! Opaque JSON-RPC frame bridge for hosts that exchange messages with an ACP client.
//!
//! The bridge uses the existing in-memory transport and does not interpret ACP methods.
//! A JavaScript host can submit a frame and await the next outbound frame; the same
//! host loop and request router used by native clients retain protocol ownership.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

use super::{
    mpsc::MpscClientTransport,
    types::{AcpError, IncomingMessage, RequestId},
    AcpTransport,
};

/// Client-side wire adapter for an existing [`MpscClientTransport`].
pub struct WireBridge {
    client: Arc<MpscClientTransport>,
    outgoing: Mutex<mpsc::UnboundedReceiver<Value>>,
    sender: Mutex<Option<mpsc::UnboundedSender<Value>>>,
}

impl Drop for WireBridge {
    fn drop(&mut self) {
        self.client.close();
    }
}

impl WireBridge {
    /// Start forwarding server-originated messages to the external host.
    /// Must be called within the same Tokio runtime as `mpsc_transport_pair`.
    pub fn new(client: MpscClientTransport) -> Self {
        let client = Arc::new(client);
        let (sender, outgoing) = mpsc::unbounded_channel();
        let reader = Arc::clone(&client);
        let writer = sender.clone();
        tokio::spawn(async move {
            while let Some(message) = reader.recv().await {
                let frame = match message {
                    IncomingMessage::Request { id, method, params } => {
                        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
                    }
                    IncomingMessage::Notification { method, params } => {
                        json!({"jsonrpc": "2.0", "method": method, "params": params})
                    }
                    IncomingMessage::Response { id, result } => response_frame(id, result),
                };
                if writer.send(frame).is_err() {
                    break;
                }
            }
        });
        Self {
            client,
            outgoing: Mutex::new(outgoing),
            sender: Mutex::new(Some(sender)),
        }
    }

    /// Submit a JSON-RPC request, notification, or response from the external client.
    /// Requests run independently so a later cancellation notification can overtake them.
    pub async fn send(&self, frame: Value) -> Result<(), AcpError> {
        let object = frame
            .as_object()
            .ok_or_else(|| invalid_frame("JSON-RPC frame must be an object"))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(invalid_frame("JSON-RPC version must be 2.0"));
        }
        if let Some(method) = object.get("method") {
            let method = method
                .as_str()
                .filter(|method| !method.is_empty())
                .ok_or_else(|| invalid_frame("JSON-RPC method must be a nonempty string"))?;
            if object.contains_key("result") || object.contains_key("error") {
                return Err(invalid_frame(
                    "JSON-RPC method frame cannot contain a response",
                ));
            }
            let params = object.get("params").cloned().unwrap_or(Value::Null);
            if let Some(id) = object.get("id") {
                let id = parse_id(id)?;
                let client = Arc::clone(&self.client);
                let sender = self
                    .sender
                    .lock()
                    .await
                    .as_ref()
                    .cloned()
                    .ok_or_else(transport_closed)?;
                let method = method.to_owned();
                tokio::spawn(async move {
                    let result = client.send_request(&method, params).await;
                    let _ = sender.send(response_frame(id, result));
                });
                Ok(())
            } else {
                self.client.send_notification(method, params).await
            }
        } else {
            let id = parse_id(
                object
                    .get("id")
                    .ok_or_else(|| invalid_frame("JSON-RPC response requires an id"))?,
            )?;
            let result = match (object.get("result"), object.get("error")) {
                (Some(result), None) => Ok(result.clone()),
                (None, Some(error)) => Err(serde_json::from_value(error.clone())
                    .map_err(|_| invalid_frame("Invalid JSON-RPC error object"))?),
                _ => {
                    return Err(invalid_frame(
                        "JSON-RPC response requires one result or error",
                    ))
                }
            };
            self.client.send_response(id, result).await
        }
    }

    /// Await a raw frame emitted by the existing ACP transport.
    pub async fn recv(&self) -> Option<Value> {
        self.outgoing.lock().await.recv().await
    }

    /// Close admission and settle any pending requests.
    pub async fn close(&self) {
        self.client.close();
        self.sender.lock().await.take();
    }
}

fn response_frame(id: RequestId, result: Result<Value, AcpError>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
}

fn parse_id(value: &Value) -> Result<RequestId, AcpError> {
    serde_json::from_value(value.clone())
        .map_err(|_| invalid_frame("JSON-RPC id must be a string or integer"))
}

fn invalid_frame(message: &str) -> AcpError {
    AcpError::new(-32600, message)
}

fn transport_closed() -> AcpError {
    AcpError::new(-32603, "Transport closed")
}

#[cfg(test)]
#[path = "wire_bridge_test.rs"]
mod tests;
