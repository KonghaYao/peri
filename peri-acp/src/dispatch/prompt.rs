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
/// 路径 `message.content` 形态对齐。`extract_prompt_params` 为单一事实源；
/// `run_prompt` / idle 挂起注入经 [`extract_and_validate_run_prompt_params`] 复用。
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

/// Merge top-level `attachments` (wire image blocks) into [`MessageContent`].
/// Clients may send images only in `attachments` instead of `message.content` blocks.
fn merge_attachments_into_content(
    content: MessageContent,
    attachments: Option<&Value>,
) -> MessageContent {
    let attachment_blocks = attachments
        .and_then(|v| v.as_array())
        .map(|arr| prompt_blocks_to_content(arr).content_blocks())
        .unwrap_or_default();
    if attachment_blocks.is_empty() {
        return content;
    }
    let mut blocks = content.content_blocks();
    blocks.extend(attachment_blocks);
    if blocks.is_empty() {
        MessageContent::text("")
    } else {
        MessageContent::Blocks(blocks)
    }
}

/// Extract prompt parameters from a JSON-RPC `session/prompt` request.
///
/// Returns `(session_id, content, attachments)` on success.
/// Top-level `attachments` image blocks are merged into `content` for agent execution.
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
    let content = merge_attachments_into_content(content, attachments.as_ref());

    Ok((session_id, content, attachments))
}

/// `run_prompt` 专用：在 [`extract_prompt_params`] 之后校验 body 键语义（与迁移前
/// `host/prompt.rs` 一致，并允许仅顶层 `attachments` 无 `message`/`prompt` 键）。
pub(crate) fn validate_run_prompt_body(
    params: &Value,
    content: &MessageContent,
) -> Result<(), AcpError> {
    if params.get("message").is_some() {
        return Ok(());
    }
    if let Some(prompt) = params.get("prompt") {
        if prompt.is_array() {
            return Ok(());
        }
        return Err(AcpError::new(-32602, "missing message"));
    }
    // 与 executor `is_keepgoing` 一致使用 `MessageContent::is_empty()`（ARC keepgoing）；
    // extract 产出形态下与迁移前 `content_blocks().is_empty()` 判定等价。
    if content.is_empty() {
        Err(AcpError::new(-32602, "missing message"))
    } else {
        Ok(())
    }
}

/// `run_prompt` 与 idle 挂起注入共用的参数管道：extract + body 守卫。
pub(crate) fn extract_and_validate_run_prompt_params(
    params: &Value,
) -> Result<(String, MessageContent, Option<Value>), AcpError> {
    let (session_id, content, attachments) = extract_prompt_params(params)?;
    validate_run_prompt_body(params, &content)?;
    Ok((session_id, content, attachments))
}

/// Handle a `session/prompt` request.
///
/// 校验 session 存在性与 **完整 prompt body 语义**（与 `run_prompt` 相同守卫）。
/// 生产 turn 执行走 `dispatch_prompt_turn` → `run_prompt`；本函数供 dispatch 层
/// 复用参数校验，不启动 executor。
///
/// Returns `Ok(serde_json::json!({}))` when the session exists and params are valid.
pub fn handle_prompt(
    params: &Value,
    session_exists: impl Fn(&str) -> bool,
) -> Result<Value, AcpError> {
    let (session_id, _content, _attachments) = extract_and_validate_run_prompt_params(params)?;

    if !session_exists(&session_id) {
        return Err(AcpError::new(-32602, "session not found"));
    }

    Ok(serde_json::json!({}))
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
