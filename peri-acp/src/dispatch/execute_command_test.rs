//! Tests for execute_command

use super::*;

#[test]
fn test_extract_params_basic() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "command": "/bg",
        "args": "do something"
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s1");
    assert_eq!(cmd, "/bg");
    assert_eq!(args.as_str().unwrap(), "do something");
}

#[test]
fn test_extract_params_session_id_underscore() {
    let params = serde_json::json!({
        "session_id": "s2",
        "command": "/compact"
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s2");
    assert_eq!(cmd, "/compact");
    assert!(args.is_null());
}

#[test]
fn test_extract_params_missing_session_id() {
    let params = serde_json::json!({
        "command": "/bg"
    });
    let err = extract_execute_command_params(&params).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("sessionId"));
}

#[test]
fn test_extract_params_missing_command() {
    let params = serde_json::json!({
        "sessionId": "s1"
    });
    let err = extract_execute_command_params(&params).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("command"));
}

#[test]
fn test_extract_params_json_args() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "command": "/rewind",
        "args": { "target_message_id": "abc", "revert_files": true }
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s1");
    assert_eq!(cmd, "/rewind");
    assert_eq!(args["target_message_id"], "abc");
    assert_eq!(args["revert_files"], true);
}
