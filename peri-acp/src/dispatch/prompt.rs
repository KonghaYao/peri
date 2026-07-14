//! `session/prompt` dispatch handler — extracts parameters, validates the session,
//! and delegates to [`crate::session::executor::run_session_loop`].
//!
//! Both TUI (MpscTransport) and stdio transport paths share this handler to avoid
//! duplicating parameter extraction and session-lookup logic.

use peri_agent::messages::MessageContent;
use serde_json::Value;

use crate::transport::types::AcpError;

/// Extract prompt parameters from a JSON-RPC `session/prompt` request.
///
/// Returns `(session_id, content, attachments)` on success.
/// The `attachments` field is accepted but currently ignored (reserved for
/// future image/file attachment support).
pub fn extract_prompt_params(
    params: &Value,
) -> Result<(String, MessageContent, Option<Value>), AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();

    let content = params
        .get("message")
        .and_then(|m| m.get("content"))
        .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
        .unwrap_or_else(|| MessageContent::text(""));

    let attachments = params.get("attachments").cloned();

    Ok((session_id, content, attachments))
}

/// Handle a `session/prompt` request.
///
/// Extracts parameters, validates the session exists, and returns `{}` on success.
/// The caller is responsible for spawning the actual execution via
/// [`crate::session::executor::run_session_loop`] (which requires a full
/// [`crate::session::executor::PromptExecutionContext`]).
///
/// Returns `Ok(serde_json::json!({}))` when the session exists and params are valid.
pub fn handle_prompt(
    params: &Value,
    session_exists: impl Fn(&str) -> bool,
) -> Result<Value, AcpError> {
    let (session_id, _content, _attachments) = extract_prompt_params(params)?;

    if !session_exists(&session_id) {
        return Err(AcpError::new(-32602, "session not found"));
    }

    Ok(serde_json::json!({}))
}

#[cfg(test)]
mod tests {
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
}
