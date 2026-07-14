use super::*;

#[test]
fn test_parse_message_response() {
    let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Response { id, result, .. } => {
            assert_eq!(id, 1);
            assert!(result.is_some());
        }
        _ => panic!("expected Response"),
    }
}

#[test]
fn test_parse_message_request_with_id() {
    let raw = r#"{"jsonrpc":"2.0","id":100,"method":"agent/run","params":{"prompt":"hi"}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Request { id, method, .. } => {
            assert_eq!(id, Some(100));
            assert_eq!(method, "agent/run");
        }
        _ => panic!("expected Request"),
    }
}

#[test]
fn test_parse_message_notification_no_id() {
    let raw = r#"{"jsonrpc":"2.0","method":"progress/event","params":{"type":"run_started"}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Request { id, method, .. } => {
            assert!(id.is_none());
            assert_eq!(method, "progress/event");
        }
        _ => panic!("expected Request (notification)"),
    }
}

#[test]
fn test_parse_message_invalid_json_returns_none() {
    assert!(parse_message("not json").is_none());
}

#[test]
fn test_parse_message_error_response() {
    let raw = r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32000,"message":"aborted"}}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Response { id, result, error } => {
            assert_eq!(id, 5);
            assert!(result.is_none());
            assert!(error.is_some());
            assert_eq!(error.unwrap().code, -32000);
        }
        _ => panic!("expected Response"),
    }
}

#[test]
fn test_parse_message_response_null_result() {
    let raw = r#"{"jsonrpc":"2.0","id":3,"result":null}"#;
    let msg = parse_message(raw).unwrap();
    match msg {
        ParsedMessage::Response { id, result, .. } => {
            assert_eq!(id, 3);
            assert!(result.is_some()); // null is still Some(Value::Null)
        }
        _ => panic!("expected Response"),
    }
}

#[test]
fn test_parse_message_no_method_no_result_returns_none() {
    // 既无 method 也无 result/error → 丢弃
    let raw = r#"{"jsonrpc":"2.0","id":7}"#;
    assert!(parse_message(raw).is_none());
}

#[test]
fn test_parse_message_empty_string_returns_none() {
    assert!(parse_message("").is_none());
}
