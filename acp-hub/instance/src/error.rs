//! JSON-RPC 错误码定义与响应构造

/// JSON-RPC 标准错误码
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// acp-hub 自定义错误码
pub const SESSION_NOT_FOUND: i64 = -32000;
pub const SESSION_CRASHED: i64 = -32001;
pub const SPAWN_FAILED: i64 = -32002;
pub const CHILD_TIMEOUT: i64 = -32003;
pub const CHILD_EXITED: i64 = -32004;

/// 构造 JSON-RPC 2.0 错误响应
///
/// - `id` 为 `None` 时响应不含 id 字段（通知错误）
/// - `id` 为 `Some(v)` 时回传原始 id
pub fn error_response(
    id: Option<&serde_json::Value>,
    code: i64,
    message: &str,
) -> serde_json::Value {
    let mut resp = serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": code,
            "message": message
        }
    });
    if let Some(id) = id {
        resp["id"] = id.clone();
    }
    resp
}

/// 构造 JSON-RPC 2.0 成功响应
pub fn ok_response(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// 从 JSON-RPC 消息中提取 method 字段
pub fn extract_method(msg: &serde_json::Value) -> Option<&str> {
    msg.get("method").and_then(|v| v.as_str())
}

/// 从 ACP 帧中提取 sessionId（§3.3 最小协议面双格式）。
///
/// 兼容两种包裹形态：
/// - 原始 `{type, payload}` 格式：`payload.sessionId`；
/// - JSON-RPC 包裹格式：`params.sessionId`（ACP v1 camelCase 规范）。
///
/// 无法提取 → `None`（调用方丢弃并记本地缺口计数，§3.3）。
pub fn extract_session_id(msg: &serde_json::Value) -> Option<&str> {
    msg.get("payload")
        .and_then(|p| p.get("sessionId"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            msg.get("params")
                .and_then(|p| p.get("sessionId"))
                .and_then(|v| v.as_str())
        })
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
