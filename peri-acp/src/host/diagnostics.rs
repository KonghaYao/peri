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
