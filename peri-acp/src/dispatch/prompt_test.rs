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

// ── 批 3 §7 #1/#2：stdio ACP 客户端 `prompt` 字段（ContentBlock wire 形态）──

/// stdio `session/prompt` 的 text block：`prompt: [{"type":"text","text":...}]`
/// → Text message content（无 `message` 字段时走兼容分支）。
#[test]
fn test_extract_prompt_params_stdio_prompt_text_blocks() {
    let params = serde_json::json!({
        "sessionId": "s-io",
        "prompt": [
            { "type": "text", "text": "hello stdio" },
            { "type": "text", "text": ", world" }
        ]
    });
    let (sid, content, attachments) = extract_prompt_params(&params).unwrap();
    assert_eq!(sid, "s-io");
    assert_eq!(
        content.text_content(),
        "hello stdio, world",
        "多 text block 应拼接（Blocks 形态 text_content 语义）"
    );
    assert!(attachments.is_none());
}

/// stdio image block：`{"type":"image","mimeType":"image/png","data":"..."}`
/// （camelCase）→ peri base64 Image block；未识别变体（resourceLink 等）跳过。
#[test]
fn test_prompt_blocks_to_content_image_and_unknown_skipped() {
    let blocks = serde_json::json!([
        { "type": "image", "mimeType": "image/png", "data": "aGk=" },
        { "type": "resourceLink", "uri": "file:///x" },
        { "type": "audio", "data": "..", "mimeType": "audio/wav" }
    ]);
    let content = super::prompt_blocks_to_content(blocks.as_array().unwrap());
    assert!(
        matches!(&content, peri_acp_types::messages::MessageContent::Blocks(b) if b.len() == 1),
        "仅 image block 被转换，其余变体跳过: {:?}",
        content
    );
    let json = serde_json::to_value(&content).unwrap();
    assert_eq!(json[0]["type"], "image");
    assert_eq!(json[0]["source"]["type"], "base64");
    assert_eq!(json[0]["source"]["media_type"], "image/png");
    assert_eq!(json[0]["source"]["data"], "aGk=");
}

/// image block 缺 mimeType/data 视为不可转换 → 跳过；空数组回落 Text("")。
#[test]
fn test_prompt_blocks_to_content_empty_or_malformed_falls_back_text() {
    let empty = serde_json::json!([]);
    let content = super::prompt_blocks_to_content(empty.as_array().unwrap());
    assert_eq!(content, peri_acp_types::messages::MessageContent::text(""));

    let malformed = serde_json::json!([
        { "type": "image", "mimeType": "image/png" }, // 缺 data
        { "type": "text" } // 缺 text
    ]);
    let content = super::prompt_blocks_to_content(malformed.as_array().unwrap());
    assert_eq!(
        content,
        peri_acp_types::messages::MessageContent::text(""),
        "全部 block 不可转换时回落空文本（与迁移前 handle_prompt 语义一致）"
    );
}

/// snake_case image 字段兜底（`mime_type`）也应被接受。
#[test]
fn test_prompt_blocks_to_content_image_snake_case_fallback() {
    let blocks = serde_json::json!([
        { "type": "image", "mime_type": "image/jpeg", "data": "cGVyaQ==" }
    ]);
    let content = super::prompt_blocks_to_content(blocks.as_array().unwrap());
    let json = serde_json::to_value(&content).unwrap();
    assert_eq!(json[0]["type"], "image");
    assert_eq!(json[0]["source"]["media_type"], "image/jpeg");
}

/// `message` 存在时优先（TUI 主路径），stdio `prompt` 不干扰。
#[test]
fn test_extract_prompt_params_message_takes_priority_over_prompt() {
    let params = serde_json::json!({
        "sessionId": "s-prio",
        "message": { "content": "from message" },
        "prompt": [ { "type": "text", "text": "from prompt" } ]
    });
    let (_, content, _) = extract_prompt_params(&params).unwrap();
    assert_eq!(content.text_content(), "from message");
}
