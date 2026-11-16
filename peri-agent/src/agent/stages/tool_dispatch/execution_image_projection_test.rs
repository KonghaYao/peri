// Responsibility-specific cases share the original test namespace and fixtures.

#[test]
fn raw_image_content_is_projected_into_model_tool_message() {
    let raw = peri_acp_types::tool_outcome::RawToolResult::new(serde_json::json!({
        "content": [
            {"type": "text", "text": "Image read"},
            {"type": "image", "data": "AQID", "mimeType": "image/png"}
        ]
    }));
    let output = ToolOutput::from_raw_result("Image read\n[image: image/png]", raw);
    let media = crate::agent::stages::tool_dispatch::execution::model_media_blocks(&output.outcome);
    assert_eq!(media.len(), 1, "必须恰好提升一个图片块");

    let settled = SettledInvocation {
        call: ToolCall::new("call-1", "Read", serde_json::json!({})),
        result: ToolResult::success("call-1", "Read", "Image read\n[image: image/png]"),
        outcome: output.outcome.clone(),
    };
    let message = crate::agent::stages::tool_dispatch::execution::settled_tool_message(&settled);
    let blocks = message.content_blocks();
    assert!(
        blocks
            .iter()
            .any(|block| matches!(block, crate::messages::ContentBlock::Text { text } if text.contains("Image read"))),
        "文本投影必须保留"
    );
    assert!(
        blocks
            .iter()
            .any(|block| matches!(block, crate::messages::ContentBlock::Image { .. })),
        "图片块必须进入模型工具消息"
    );
}

#[test]
fn unsupported_or_empty_tool_images_are_dropped_without_losing_text() {
    for block in [
        serde_json::json!({"type": "image", "data": "AQID", "mimeType": "image/svg+xml"}),
        serde_json::json!({"type": "image", "data": "", "mimeType": "image/png"}),
    ] {
        let raw = peri_acp_types::tool_outcome::RawToolResult::new(serde_json::json!({
            "content": [block]
        }));
        let outcome = ToolOutcome::from_raw(raw);
        assert!(
            crate::agent::stages::tool_dispatch::execution::model_media_blocks(&outcome).is_empty(),
            "不支持的图片必须被丢弃"
        );
        let settled = SettledInvocation {
            call: ToolCall::new("call-3", "Read", serde_json::json!({})),
            result: ToolResult::success("call-3", "Read", "Image read"),
            outcome,
        };
        let message =
            crate::agent::stages::tool_dispatch::execution::settled_tool_message(&settled);
        assert!(
            matches!(message.message_content(), MessageContent::Text(text) if text == "Image read"),
            "丢弃图片后必须保留文本投影"
        );
    }
}

#[test]
fn aggregate_image_payload_budget_is_shared_across_one_tool_message() {
    let chunk = "A".repeat(8 * 1024 * 1024);
    let raw = peri_acp_types::tool_outcome::RawToolResult::new(serde_json::json!({
        "content": [
            {"type": "image", "data": chunk.clone(), "mimeType": "image/png"},
            {"type": "image", "data": chunk, "mimeType": "image/png"}
        ]
    }));
    let outcome = ToolOutcome::from_raw(raw);
    assert_eq!(
        crate::agent::stages::tool_dispatch::execution::model_media_blocks(&outcome).len(),
        1,
        "第二张图不得让同一条工具消息的 base64 总量超过预算"
    );
}

#[test]
fn text_only_tool_results_keep_the_legacy_text_message_shape() {
    let settled = SettledInvocation::unreported(
        ToolCall::new("call-2", "Read", serde_json::json!({})),
        ToolResult::success("call-2", "Read", "plain text"),
    );
    let message = crate::agent::stages::tool_dispatch::execution::settled_tool_message(&settled);
    assert!(
        matches!(message.message_content(), MessageContent::Text(text) if text == "plain text"),
        "无图片时不得改变既有文本消息形状"
    );
}
