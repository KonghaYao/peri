//! `session/prompt` dispatch handler — extracts parameters, validates the session,
//! and delegates to [`crate::session::executor::run_session_loop`].
//!
//! Both TUI (MpscTransport) and stdio transport paths share this handler to avoid
//! duplicating parameter extraction and session-lookup logic.

use peri_acp_types::messages::{ContentBlock, MessageContent};
use serde_json::Value;

use crate::transport::types::AcpError;

/// 解析 stdio/ACP 客户端 `session/prompt` 的 `prompt` 字段（agent-client-protocol-
/// schema `PromptRequest` 的 wire 形态：`prompt: Vec<ContentBlock>`，外部封装
/// camelCase，block 内 `type` 判别）为 peri 的 [`MessageContent`]——与 TUI/notify
/// 路径 `message.content` 形态对齐（批 3 §7 #1/#2 合并方向：以 run_prompt 为
/// 基座、兼容 stdio 输入）。
///
/// 转换规则与迁移前 `host/stdio/session/prompt.rs` 一致：
/// - `{"type":"text","text":...}` → Text block；
/// - `{"type":"image","data":...,"mimeType":...}`（camelCase；snake_case 兜底）
///   → base64 Image block；
/// - 其余变体（Audio/ResourceLink/Resource 等）不解析、跳过；
/// - 无可转换 block 时回落 `MessageContent::text("")`。
pub(crate) fn prompt_blocks_to_content(blocks: &[Value]) -> MessageContent {
    let converted: Vec<ContentBlock> = blocks
        .iter()
        .filter_map(|block| match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => block
                .get("text")
                .and_then(|v| v.as_str())
                .map(ContentBlock::text),
            Some("image") => {
                let mime = block
                    .get("mimeType")
                    .or_else(|| block.get("mime_type"))
                    .and_then(|v| v.as_str());
                let data = block.get("data").and_then(|v| v.as_str());
                match (mime, data) {
                    (Some(mime), Some(data)) => Some(ContentBlock::image_base64(mime, data)),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    if converted.is_empty() {
        MessageContent::text("")
    } else {
        MessageContent::Blocks(converted)
    }
}

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

    // TUI/notify 路径：`message.content`（MessageContent 形态）；
    // stdio ACP 客户端：`prompt`（`Vec<ContentBlock>` wire 形态，兼容分支）。
    let content = if let Some(message) = params.get("message") {
        message
            .get("content")
            .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
            .unwrap_or_else(|| MessageContent::text(""))
    } else {
        params
            .get("prompt")
            .and_then(|v| v.as_array())
            .map(|blocks| prompt_blocks_to_content(blocks))
            .unwrap_or_else(|| MessageContent::text(""))
    };

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
