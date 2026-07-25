use super::*;

#[test]
fn test_from_base_messages_basic() {
    let msgs = vec![BaseMessage::human("Hello"), BaseMessage::ai("Hi")];
    let val = AnthropicAdapter::from_base_messages(&msgs);
    let arr = val.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[1]["role"], "assistant");
}

#[test]
fn test_from_base_messages_tool_use_merged() {
    let msgs = vec![
        BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                "tc1",
                "Bash",
                json!({"command": "ls"}),
            )],
        ),
        BaseMessage::tool_result("tc1", "file.txt"),
    ];
    let val = AnthropicAdapter::from_base_messages(&msgs);
    let arr = val.as_array().unwrap();
    // tool result 应合并到 user 消息
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["role"], "assistant");
    // 第二条 - tool result 合并为 user
    assert_eq!(arr[1]["role"], "user");
    let content = arr[1]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "tool_result");
}

#[test]
fn test_system_extracted() {
    let msgs = vec![
        BaseMessage::system("You are helpful"),
        BaseMessage::human("Hello"),
    ];
    let (msgs_val, system) = AnthropicAdapter::to_anthropic_with_system(&msgs);
    assert_eq!(system.as_deref(), Some("You are helpful"));
    // system 消息不进入 messages 数组
    assert_eq!(msgs_val.len(), 1);
    assert_eq!(msgs_val[0]["role"], "user");
}

#[test]
fn test_to_base_message_assistant_with_tool_use() {
    let val = json!({
        "role": "assistant",
        "content": [
            { "type": "text", "text": "I'll run bash" },
            { "type": "tool_use", "id": "tc1", "name": "Bash", "input": {"command": "ls"} }
        ]
    });
    let msg = AnthropicAdapter::to_base_message(&val).unwrap();
    assert!(msg.has_tool_calls());
    assert_eq!(msg.tool_calls()[0].name, "Bash");
}

#[test]
fn test_to_base_message_roundtrip() {
    let original = BaseMessage::human("Test");
    let val = AnthropicAdapter::from_base_messages(&[original]);
    let arr = val.as_array().unwrap();
    let restored = AnthropicAdapter::to_base_message(&arr[0]).unwrap();
    assert_eq!(restored.content(), "Test");
}

/// 双写一致性 roundtrip：Ai 消息经过序列化→API→反序列化后，
/// content blocks 中的 ToolUse 与 tool_calls 字段始终保持同步
#[test]
fn test_tool_calls_dual_write_roundtrip() {
    // 构造包含工具调用的 AI 消息（模拟 LLM 响应解析后的内部状态）
    let original = BaseMessage::ai_from_blocks(vec![
        ContentBlock::text("I'll run bash"),
        ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
    ]);
    assert!(original.has_tool_calls());
    assert_eq!(original.tool_calls().len(), 1);

    // 序列化为 Anthropic API 格式
    let api_json = AnthropicAdapter::from_base_messages(&[original]);
    let arr = api_json.as_array().unwrap();
    let assistant_msg = &arr[0];
    assert_eq!(assistant_msg["role"], "assistant");

    // API 格式应包含 tool_use block
    let blocks = assistant_msg["content"].as_array().unwrap();
    let has_tool_use = blocks.iter().any(|b| b["type"] == "tool_use");
    assert!(has_tool_use, "序列化后 content 应包含 tool_use block");

    // 反序列化回 BaseMessage，双写应仍然一致
    let restored = AnthropicAdapter::to_base_message(assistant_msg).unwrap();
    assert!(restored.has_tool_calls(), "反序列化后 tool_calls 应保留");
    assert_eq!(restored.tool_calls().len(), 1);
    assert_eq!(restored.tool_calls()[0].id, "tc1");
    assert_eq!(restored.tool_calls()[0].name, "Bash");

    // content blocks 中也应有 ToolUse（双写一致性验证）
    let content_has_tool_use = restored
        .content_blocks()
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    assert!(content_has_tool_use, "content blocks 中应有 ToolUse block");
}

/// Text 类型内容 + tool_calls 的序列化：应从 tool_calls 重建 ToolUse blocks
#[test]
fn test_text_content_with_tool_calls_serializes_correctly() {
    let msg = BaseMessage::ai_with_tool_calls(
        "I'll run bash",
        vec![ToolCallRequest::new(
            "tc2",
            "Bash",
            json!({"command": "pwd"}),
        )],
    );
    let api_json = AnthropicAdapter::from_base_messages(&[msg]);
    let arr = api_json.as_array().unwrap();
    let blocks = arr[0]["content"].as_array().unwrap();

    let text_block = blocks.iter().find(|b| b["type"] == "text");
    let tool_block = blocks.iter().find(|b| b["type"] == "tool_use");
    assert!(text_block.is_some(), "应包含 text block");
    assert!(tool_block.is_some(), "应从 tool_calls 重建 tool_use block");
    assert_eq!(tool_block.unwrap()["id"], "tc2");
}

/// 验证连续 Tool 消息合并后顺序保持不变
#[test]
fn test_tool_results_order_preserved() {
    let msgs = vec![
        BaseMessage::ai_with_tool_calls(
            "",
            vec![
                ToolCallRequest::new("t1", "Read", json!({"file_path": "a.rs"})),
                ToolCallRequest::new("t2", "Read", json!({"file_path": "b.rs"})),
            ],
        ),
        BaseMessage::tool_result("t1", "content a"),
        BaseMessage::tool_result("t2", "content b"),
    ];
    let val = AnthropicAdapter::from_base_messages(&msgs);
    let arr = val.as_array().unwrap();

    // tool results 应合并到第二条 user 消息
    let user_msg = &arr[1];
    assert_eq!(user_msg["role"], "user");
    let content = user_msg["content"].as_array().unwrap();
    let results: Vec<_> = content
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "t1");
    assert_eq!(results[1]["tool_use_id"], "t2");
}

/// 投影后的内容（Image→Text 占位符）在 Anthropic 适配器中正确序列化
#[test]
fn test_projected_content_with_placeholder_serializes() {
    // 模拟 render_llm_view 投影后的 Human 消息：
    // 原始 Image block 被 ReplaceMedia 转为 Text 占位块
    let projected = BaseMessage::human(MessageContent::Blocks(vec![
        ContentBlock::text("What's in this image?"),
        ContentBlock::text("[图片已压缩: image]"),
    ]));
    let val = AnthropicAdapter::from_base_messages(&[projected]);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["role"], "user");

    let content = arr[0]["content"].as_array().expect("content 应为 array");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "[图片已压缩: image]");

    // 验证不含任何 base64 image source
    let has_image_src = content.iter().any(|b| {
        b["source"]
            .as_object()
            .map(|s| s.get("type").and_then(|v| v.as_str()) == Some("base64"))
            .unwrap_or(false)
    });
    assert!(
        !has_image_src,
        "投影后 content 不应包含 base64 image source"
    );
}

/// 投影后的 ToolResult（CompactToolResult head/tail 截断）正确序列化
#[test]
fn test_compacted_tool_result_serializes() {
    let msgs = vec![
        BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                "tc_1",
                "Bash",
                json!({"command": "ls"}),
            )],
        ),
        BaseMessage::tool_result(
            "tc_1",
            "AAAA".repeat(50) + "\n... [字符已省略] ...\n" + &"BBBB".repeat(25),
        ),
    ];
    let val = AnthropicAdapter::from_base_messages(&msgs);
    let arr = val.as_array().unwrap();

    // tool result 应合并到 user 消息
    let user_content = arr[1]["content"].as_array().unwrap();
    let tool_result = user_content
        .iter()
        .find(|b| b["type"] == "tool_result")
        .unwrap();
    assert_eq!(tool_result["tool_use_id"], "tc_1");
    assert!(!tool_result["is_error"].as_bool().unwrap());

    // content 包含截断文本（可能是字符串或数组）
    let result_text = if tool_result["content"].is_string() {
        tool_result["content"].as_str().unwrap().to_string()
    } else if tool_result["content"].is_array() {
        let arr = tool_result["content"].as_array().unwrap();
        arr[0]["text"].as_str().unwrap().to_string()
    } else {
        panic!("unexpected content type");
    };
    assert!(
        result_text.contains("字符已省略"),
        "应包含截断标记，实际: {}",
        result_text
    );
}

/// 带签名 reasoning 在 Anthropic 适配器中保留 signature 字段
#[test]
fn test_signed_reasoning_preserves_signature() {
    let msg = BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning_with_signature("thinking process", "sig_abc123"),
        ContentBlock::text("final answer"),
    ]);
    let val = AnthropicAdapter::from_base_messages(&[msg]);
    let arr = val.as_array().unwrap();
    let content = arr[0]["content"].as_array().unwrap();

    let thinking_block = content.iter().find(|b| b["type"] == "thinking").unwrap();
    assert_eq!(thinking_block["signature"], "sig_abc123");
    assert_eq!(thinking_block["thinking"], "thinking process");
}
