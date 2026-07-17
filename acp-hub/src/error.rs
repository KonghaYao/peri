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

/// 从 JSON-RPC 消息的 params 中提取 session_id
pub fn extract_session_id(msg: &serde_json::Value) -> Option<&str> {
    msg.get("params")
        .and_then(|p| p.get("session_id"))
        .and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_response_with_id() {
        let id = serde_json::json!(1);
        let resp = error_response(Some(&id), SESSION_NOT_FOUND, "session not found");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["error"]["code"], -32000);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not found"));
    }

    #[test]
    fn test_error_response_without_id() {
        let resp = error_response(None, PARSE_ERROR, "parse error");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp.get("id").is_none() || resp["id"].is_null());
        assert_eq!(resp["error"]["code"], -32700);
    }

    #[test]
    fn test_extract_method() {
        let msg = serde_json::json!({"method": "session/new"});
        assert_eq!(extract_method(&msg), Some("session/new"));

        let no_method = serde_json::json!({"id": 1});
        assert_eq!(extract_method(&no_method), None);
    }

    #[test]
    fn test_extract_session_id() {
        let msg = serde_json::json!({
            "method": "prompt",
            "params": {"session_id": "abc-123"}
        });
        assert_eq!(extract_session_id(&msg), Some("abc-123"));

        let no_sid = serde_json::json!({"method": "prompt", "params": {}});
        assert_eq!(extract_session_id(&no_sid), None);
    }

    #[test]
    fn test_ok_response() {
        let id = serde_json::json!("req-1");
        let result = serde_json::json!({"session_id": "abc"});
        let resp = ok_response(&id, result);
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], "req-1");
        assert_eq!(resp["result"]["session_id"], "abc");
    }
}
