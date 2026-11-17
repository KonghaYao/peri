use serde_json::Value;

use crate::transport::types::AcpError;
use crate::transport::types::RequestId;
use crate::transport::AcpTransport;

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
        if self.method.starts_with("session/input/") {
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

/// 成功响应包络在 `result` 对象之外的字节数（`{"jsonrpc":"2.0","id":<id>,"result":}`）。
///
/// id 由客户端提供、长度无协议上限，`session/load` 的首屏预算必须按**实际**请求 id
/// 计入包络，不能只序列化 result 对象。
pub(super) fn response_envelope_bytes(id: &RequestId) -> u64 {
    let id = match id {
        RequestId::String(value) => Value::String(value.clone()),
        RequestId::Number(value) => Value::Number((*value).into()),
    };
    let probe = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": null,
    });
    serde_json::to_vec(&probe)
        // `null` 占位符被实际 result 对象替换。
        .map(|bytes| bytes.len().saturating_sub(4) as u64)
        .unwrap_or_default()
}

/// `session/load` 的后置 `_meta["peri.recovery"]` 可能把已按预算定长的响应推过
/// `maxResponseBytes`。历史首屏是必需载荷、recovery 只是附加事实：超限时移除
/// recovery 元数据，保证单响应不越过协商上限。返回是否发生了移除。
pub(super) fn drop_recovery_meta_if_over_budget(
    result: &mut Result<Value, AcpError>,
    max_bytes: u64,
    envelope_bytes: u64,
) -> bool {
    let Ok(response) = result else {
        return false;
    };
    let has_recovery = response
        .get("_meta")
        .and_then(Value::as_object)
        .is_some_and(|meta| meta.contains_key("peri.recovery"));
    if !has_recovery {
        return false;
    }
    let over_budget = serde_json::to_vec(&*response)
        .map(|bytes| (bytes.len() as u64).saturating_add(envelope_bytes) > max_bytes)
        .unwrap_or(false);
    if !over_budget {
        return false;
    }
    if let Some(meta) = response.get_mut("_meta").and_then(Value::as_object_mut) {
        meta.remove("peri.recovery");
    }
    true
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
