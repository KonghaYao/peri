//! Tests for error_hub

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
        "method": "session/prompt",
        "params": {"sessionId": "abc-123"}
    });
    assert_eq!(extract_session_id(&msg), Some("abc-123"));

    let no_sid = serde_json::json!({"method": "session/prompt", "params": {}});
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
