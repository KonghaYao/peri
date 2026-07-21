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
#[path = "prompt_test.rs"]
mod tests;
