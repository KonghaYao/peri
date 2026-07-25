use super::*;

#[test]
fn test_from_base_messages_human_ai() {
    let msgs = vec![BaseMessage::human("Hello"), BaseMessage::ai("Hi")];
    let val = OpenAiAdapter::from_base_messages(&msgs);
    let arr = val.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[1]["role"], "assistant");
}

#[test]
fn test_from_base_messages_system_prepended() {
    let msgs = vec![
        BaseMessage::system("You are helpful"),
        BaseMessage::human("Hello"),
    ];
    let val = OpenAiAdapter::from_base_messages(&msgs);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["role"], "system");
    assert_eq!(arr[0]["content"], "You are helpful");
}

#[test]
fn test_from_base_messages_tool() {
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
    let val = OpenAiAdapter::from_base_messages(&msgs);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["role"], "assistant");
    assert!(arr[0]["tool_calls"].is_array());
    assert_eq!(arr[1]["role"], "tool");
    assert_eq!(arr[1]["tool_call_id"], "tc1");
}

#[test]
fn test_to_base_message_roundtrip() {
    let original = BaseMessage::human("Test message");
    let val = OpenAiAdapter::from_base_messages(&[original]);
    let arr = val.as_array().unwrap();
    let restored = OpenAiAdapter::to_base_message(&arr[0]).unwrap();
    assert_eq!(restored.content(), "Test message");
}

#[test]
fn test_to_base_message_tool() {
    let val = json!({
        "role": "tool",
        "tool_call_id": "tc1",
        "content": "result"
    });
    let msg = OpenAiAdapter::to_base_message(&val).unwrap();
    if let BaseMessage::Tool { tool_call_id, .. } = msg {
        assert_eq!(tool_call_id, "tc1");
    } else {
        unreachable!("期望 Tool 消息");
    }
}

/// 双写一致性 roundtrip：从 OpenAI API 响应解析后，
/// content blocks 中的 ToolUse 与 tool_calls 字段始终同步
#[test]
fn test_tool_calls_dual_write_roundtrip() {
    // 模拟 OpenAI API 返回包含工具调用的 assistant 消息
    let api_response = json!({
        "role": "assistant",
        "content": "I'll run bash",
        "tool_calls": [{
            "id": "tc1",
            "type": "function",
            "function": {
                "name": "Bash",
                "arguments": "{\"command\":\"ls\"}"
            }
        }]
    });

    let msg = OpenAiAdapter::to_base_message(&api_response).unwrap();

    // tool_calls 字段应正确提取
    assert!(msg.has_tool_calls());
    assert_eq!(msg.tool_calls().len(), 1);
    assert_eq!(msg.tool_calls()[0].id, "tc1");
    assert_eq!(msg.tool_calls()[0].name, "Bash");

    // content blocks 中也应有 ToolUse（双写一致）
    let has_tool_use = msg
        .content_blocks()
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    assert!(has_tool_use, "content blocks 中应有 ToolUse block");

    // 序列化回 OpenAI 格式后，tool_calls 字段应存在（OpenAI 用 tool_calls 字段，不在 content 里）
    let re_serialized = OpenAiAdapter::from_base_messages(&[msg]);
    let arr = re_serialized.as_array().unwrap();
    // system prompt prepended if any, here just one assistant msg
    let assistant = arr.iter().find(|m| m["role"] == "assistant").unwrap();
    assert!(assistant["tool_calls"].is_array());
    assert_eq!(assistant["tool_calls"][0]["id"], "tc1");
    // OpenAI content 不含 ToolUse（已过滤），只保留 text
    let content_has_tool_use = assistant["content"]
        .as_array()
        .map(|arr| arr.iter().any(|b| b["type"] == "tool_use"))
        .unwrap_or(false);
    assert!(
        !content_has_tool_use,
        "OpenAI content 中不应出现 ToolUse block"
    );
}

/// Reasoning block 从 content 中过滤，但回传为 reasoning_content 顶层字段
#[test]
fn test_reasoning_block_filtered_but_preserved() {
    let msg = BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning("step 1: analyze first"),
        ContentBlock::text("final answer"),
    ]);
    let val = OpenAiAdapter::from_base_messages(&[msg]);
    let arr = val.as_array().unwrap();
    let assistant = arr.iter().find(|m| m["role"] == "assistant").unwrap();
    // reasoning 从 content 中过滤，只剩 text
    let content = assistant["content"].as_array().expect("content 应为 array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "final answer");
    // reasoning_content 顶层字段回传
    assert_eq!(assistant["reasoning_content"], "step 1: analyze first");
}

/// Reasoning + tool_calls 序列化：reasoning 回传到顶层，tool_calls 在顶层
#[test]
fn test_reasoning_with_tool_calls_serialization() {
    let msg = BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning("need bash"),
        ContentBlock::text("running..."),
        ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
    ]);
    let val = OpenAiAdapter::from_base_messages(&[msg]);
    let arr = val.as_array().unwrap();
    let assistant = arr.iter().find(|m| m["role"] == "assistant").unwrap();
    // content 只包含 text，不含 thinking 或 tool_use
    let content = assistant["content"].as_array().expect("content 应为 array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "running...");
    // reasoning_content 顶层字段
    assert_eq!(assistant["reasoning_content"], "need bash");
    // tool_calls 在顶层
    assert!(assistant["tool_calls"].is_array());
    assert_eq!(assistant["tool_calls"][0]["id"], "tc1");
}

/// 仅 reasoning block（无 text）→ content 为空字符串，reasoning_content 回传
#[test]
fn test_reasoning_only_serialization() {
    let msg = BaseMessage::ai_from_blocks(vec![ContentBlock::reasoning("thinking only")]);
    let val = OpenAiAdapter::from_base_messages(&[msg]);
    let arr = val.as_array().unwrap();
    let assistant = arr.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(assistant["content"], json!(""));
    assert_eq!(assistant["reasoning_content"], "thinking only");
}

/// 无 reasoning block 的消息应有空 reasoning_content
#[test]
fn test_no_reasoning_empty_reasoning_content() {
    let msg = BaseMessage::ai("just text");
    let val = OpenAiAdapter::from_base_messages(&[msg]);
    let arr = val.as_array().unwrap();
    let assistant = arr.iter().find(|m| m["role"] == "assistant").unwrap();
    // content 为纯文本字符串，非 array
    assert!(assistant["content"].is_string());
    // reasoning_content 应为空字符串
    assert_eq!(assistant["reasoning_content"], "");
}

/// 投影后的内容（Image→Text 占位符）在 OpenAI 适配器中正确序列化
#[test]
fn test_projected_content_with_placeholder_serializes() {
    // 模拟 render_llm_view 投影后的 Human 消息：
    // 原始 Image block 被 ReplaceMedia 转为 Text 占位块
    let projected = BaseMessage::human(MessageContent::Blocks(vec![
        ContentBlock::text("What's in this image?"),
        ContentBlock::text("[图片已压缩: image]"),
    ]));
    let val = OpenAiAdapter::from_base_messages(&[projected]);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["role"], "user");

    let content = arr[0]["content"].as_array().expect("content 应为 array");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "[图片已压缩: image]");

    // 验证不含任何 Base64 / image_url 类型
    let has_image = content.iter().any(|b| b["type"] == "image_url");
    assert!(!has_image, "投影后 content 不应包含 image_url");
}

/// 投影后的 ToolResult（CompactToolResult head/tail 截断）正确序列化
#[test]
fn test_compacted_tool_result_serializes() {
    let msg = BaseMessage::tool_result(
        "tc_1",
        "AAAA".repeat(50) + "\n... [字符已省略] ...\n" + &"BBBB".repeat(25),
    );
    let val = OpenAiAdapter::from_base_messages(&[msg]);
    let arr = val.as_array().unwrap();
    assert_eq!(arr[0]["role"], "tool");
    assert_eq!(arr[0]["tool_call_id"], "tc_1");
    assert!(arr[0]["content"].as_str().unwrap().contains("字符已省略"));
}
