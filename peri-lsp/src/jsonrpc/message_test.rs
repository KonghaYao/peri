//! Tests for message_jrpc

use super::*;

#[test]
fn test_request_serialization() {
    let req = JsonRpcRequest::new(1, "initialize", Some(serde_json::json!({"test": true})));
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains(r#""jsonrpc":"2.0""#));
    assert!(json.contains(r#""id":1"#));
    assert!(json.contains(r#""method":"initialize""#));
    assert!(json.contains(r#""test":true"#));
}

#[test]
fn test_notification_serialization() {
    let notif = JsonRpcNotification::new("initialized", None);
    let json = serde_json::to_string(&notif).unwrap();
    assert!(json.contains(r#""jsonrpc":"2.0""#));
    assert!(!json.contains(r#""id""#));
    assert!(json.contains(r#""method":"initialized""#));
}

#[test]
fn test_response_deserialization_success() {
    let json = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#;
    let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
    assert!(resp.result.is_some());
    assert!(resp.error.is_none());
}

#[test]
fn test_response_deserialization_error() {
    let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
    let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
    assert!(resp.result.is_none());
    let err = resp.error.unwrap();
    assert_eq!(err.code, -32601);
    assert_eq!(err.message, "Method not found");
}

#[test]
fn test_response_deserialization_notification_response() {
    // 某些 LSP 服务器对通知也返回无 id 的响应
    let json = r#"{"jsonrpc":"2.0","result":null}"#;
    let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
    assert!(resp.id.is_none());
}
