use serde_json::Value;

use crate::transport::{
    types::{AcpError, RequestId},
    AcpTransport,
};

#[derive(Clone)]
pub(super) struct ResponseDiagnostics {
    id: RequestId,
    method: String,
    session_id: Option<String>,
    command_id: Option<String>,
    input_id: Option<String>,
    started: std::time::Instant,
}

impl ResponseDiagnostics {
    pub(super) fn new(id: RequestId, method: &str, params: &Value) -> Self {
        Self {
            id,
            method: method.to_owned(),
            session_id: params
                .get("sessionId")
                .or_else(|| params.get("session_id"))
                .or_else(|| params.get("ownerSessionId"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            command_id: params
                .get("commandId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            input_id: params
                .get("inputId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            started: peri_time::monotonic_now(),
        }
    }

    pub(super) async fn send(
        self,
        transport: &dyn AcpTransport,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        if let Err(error) = &result {
            match error.code {
                -32800 => {
                    tracing::debug!(method = %self.method, rpc_id = %self.id,
                        session_id = self.session_id.as_deref(), code = error.code,
                        error = %error.message, "ACP request cancelled")
                }
                -32603 => {
                    tracing::error!(method = %self.method, rpc_id = %self.id,
                        session_id = self.session_id.as_deref(), code = error.code,
                        error = %error.message, "ACP request failed")
                }
                _ => {
                    tracing::warn!(method = %self.method, rpc_id = %self.id,
                        session_id = self.session_id.as_deref(), code = error.code,
                        error = %error.message, "ACP request failed")
                }
            }
        }
        let sent = transport.send_response(self.id.clone(), result).await;
        if self.method.starts_with("session/input/") || self.method.starts_with("session/work/") {
            tracing::info!(target: "perf.input", method = %self.method, rpc_id = %self.id,
                session_id = self.session_id.as_deref(), command_id = self.command_id.as_deref(),
                input_id = self.input_id.as_deref(), elapsed_us = peri_time::elapsed_since(self.started).as_micros() as u64,
                response_sent = sent.is_ok(), "ACP input request completed");
        }
        if let Err(error) = &sent {
            tracing::warn!(method = %self.method, rpc_id = %self.id,
                session_id = self.session_id.as_deref(), code = error.code,
                error = %error.message, "ACP response delivery failed");
        }
        sent
    }
}

pub(super) async fn send_session_update(
    transport: &dyn AcpTransport,
    session_id: &str,
    update_kind: &str,
    payload: Value,
) {
    if let Err(error) = transport.send_notification("session/update", payload).await {
        tracing::error!(
            method = "session/update",
            session_id,
            update_kind,
            code = error.code,
            error = %error.message,
            "ACP notification delivery failed"
        );
    }
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
