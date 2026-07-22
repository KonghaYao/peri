//! Tests for prompt

use super::*;

#[test]
fn test_extract_prompt_params_basic() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "message": { "content": "hello" }
    });
    let (sid, content, attachments) = extract_prompt_params(&params).unwrap();
    assert_eq!(sid, "s1");
    assert_eq!(content.text_content(), "hello");
    assert!(attachments.is_none());
}

#[test]
fn test_extract_prompt_params_with_attachments() {
    let params = serde_json::json!({
        "session_id": "s2",
        "message": { "content": "look at this" },
        "attachments": [{"type": "image", "data": "abc"}]
    });
    let (sid, content, attachments) = extract_prompt_params(&params).unwrap();
    assert_eq!(sid, "s2");
    assert_eq!(content.text_content(), "look at this");
    assert!(attachments.is_some());
}

#[test]
fn test_extract_prompt_params_missing_session_id() {
    let params = serde_json::json!({
        "message": { "content": "hello" }
    });
    let err = extract_prompt_params(&params).unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
fn test_extract_prompt_params_missing_message() {
    let params = serde_json::json!({
        "sessionId": "s1"
    });
    let (sid, content, attachments) = extract_prompt_params(&params).unwrap();
    assert_eq!(sid, "s1");
    // 缺少 message 时 content 默认为空文本
    assert_eq!(content.text_content(), "");
    assert!(attachments.is_none());
}

#[test]
fn test_handle_prompt_success() {
    let params = serde_json::json!({
        "sessionId": "existing",
        "message": { "content": "hello" }
    });
    let result = handle_prompt(&params, |sid| sid == "existing").unwrap();
    assert_eq!(result, serde_json::json!({}));
}

#[test]
fn test_handle_prompt_session_not_found() {
    let params = serde_json::json!({
        "sessionId": "missing",
        "message": { "content": "hello" }
    });
    let err = handle_prompt(&params, |_sid| false).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("session not found"));
}

#[test]
fn test_handle_prompt_missing_session_id() {
    let params = serde_json::json!({
        "message": { "content": "hello" }
    });
    let err = handle_prompt(&params, |_sid| true).unwrap_err();
    assert_eq!(err.code, -32602);
}
