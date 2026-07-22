//! Tests for stdio

use super::*;

#[test]
fn test_envelope_roundtrip_response() {
    let json = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok"}}"#;
    let envelope: JsonRpcEnvelope = serde_json::from_str(json).unwrap();
    assert_eq!(envelope.jsonrpc, "2.0");
    assert_eq!(envelope.id, Some(Value::Number(1.into())));
    assert!(envelope.result.is_some());
    assert!(envelope.error.is_none());
    let back = serde_json::to_string(&envelope).unwrap();
    assert!(back.contains("\"result\""));
}

#[test]
fn test_envelope_roundtrip_request() {
    let json = r#"{"jsonrpc":"2.0","id":42,"method":"session/prompt","params":{"msg":"hi"}}"#;
    let envelope: JsonRpcEnvelope = serde_json::from_str(json).unwrap();
    assert_eq!(envelope.method.as_deref(), Some("session/prompt"));
}

#[test]
fn test_envelope_roundtrip_notification() {
    let json = r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"session_id":"s1"}}"#;
    let envelope: JsonRpcEnvelope = serde_json::from_str(json).unwrap();
    assert!(envelope.id.is_none());
    assert_eq!(envelope.method.as_deref(), Some("session/cancel"));
}

#[test]
fn test_request_id_conversion() {
    let v = Value::Number(42.into());
    let id = value_to_request_id(&v);
    assert_eq!(id, RequestId::Number(42));
    let back = request_id_to_value(&id);
    assert_eq!(back, v);
}
