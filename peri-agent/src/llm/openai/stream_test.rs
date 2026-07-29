use super::*;
use crate::{
    llm::types::StopReason,
    messages::{ContentBlock, ToolCallRequest},
};

/// 流式 ToolUse 场景：content_text 应在最终 BaseMessage 中保留为 Text block。
#[test]
fn test_build_stream_response_tooluse_preserves_text() {
    let tc = ToolCallRequest::new("tc-1", "Bash", serde_json::json!({"command": "ls"}));

    let response = build_stream_response(
        "", // reasoning 为空
        "Let me run a command.",
        vec![tc],
        StopReason::ToolUse,
        None, // usage
        None, // request_id
        None, // first_token_time
    );

    let blocks = response.message.content_blocks();
    let has_text = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. }));
    let has_tool_use = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

    assert!(
        has_text,
        "流式 ToolUse 分支应保留文本块，但 blocks 中无 Text"
    );
    assert!(has_tool_use, "应包含 ToolUse block");

    let text = blocks
        .iter()
        .filter_map(|b| b.as_text())
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(text, "Let me run a command.", "文本内容应完整保留");
}

/// 流式 ToolUse 场景：content_text 为空时不应凭空产生 text block。
#[test]
fn test_build_stream_response_tooluse_empty_text() {
    let tc = ToolCallRequest::new("tc-1", "Bash", serde_json::json!({"command": "ls"}));

    let response = build_stream_response(
        "", // content_text 为空
        "",
        vec![tc],
        StopReason::ToolUse,
        None,
        None,
        None, // first_token_time
    );

    let blocks = response.message.content_blocks();
    let has_text = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if !text.is_empty()));
    assert!(!has_text, "content_text 为空时不应产生非空文本块");

    let has_tool_use = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    assert!(has_tool_use, "应包含 ToolUse block");
}

/// 流式 ToolUse 场景：含 reasoning 时文本和 tool_use 都应保留。
#[test]
fn test_build_stream_response_tooluse_with_reasoning() {
    let tc = ToolCallRequest::new("tc-1", "Grep", serde_json::json!({"pattern": "foo"}));

    let response = build_stream_response(
        "step-by-step analysis",  // reasoning_text
        "I will search for foo.", // content_text
        vec![tc],
        StopReason::ToolUse,
        None,
        None,
        None, // first_token_time
    );

    let blocks = response.message.content_blocks();
    assert!(blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Reasoning { .. })));
    assert!(blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. })));
    assert!(blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. })));
}
