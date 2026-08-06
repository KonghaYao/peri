use super::*;

#[test]
fn test_ai_from_blocks_extracts_tool_calls() {
    let blocks = vec![
        ContentBlock::text("I'll use a tool"),
        ContentBlock::tool_use("id1", "Bash", serde_json::json!({"command": "ls"})),
    ];
    let msg = BaseMessage::ai_from_blocks(blocks);
    assert!(msg.has_tool_calls());
    assert_eq!(msg.tool_calls().len(), 1);
    assert_eq!(msg.tool_calls()[0].name, "Bash");
}

#[test]
fn test_base_message_content_blocks_lazy_parse() {
    let msg = BaseMessage::ai(MessageContent::Blocks(vec![
        ContentBlock::reasoning("thinking..."),
        ContentBlock::text("answer"),
    ]));
    let blocks = msg.content_blocks();
    assert_eq!(blocks.len(), 2);
    assert!(matches!(blocks[0], ContentBlock::Reasoning { .. }));
    assert_eq!(blocks[1].as_text(), Some("answer"));
}

#[test]
fn test_human_message_multimodal() {
    let msg = BaseMessage::human(MessageContent::Blocks(vec![
        ContentBlock::text("What's in this image?"),
        ContentBlock::image_url("https://example.com/image.jpg"),
    ]));
    let blocks = msg.content_blocks();
    assert_eq!(blocks.len(), 2);
    assert!(matches!(blocks[1], ContentBlock::Image { .. }));
}

#[test]
fn test_message_id_generated() {
    // 不同消息的 id 应不同
    let m1 = BaseMessage::human("hello");
    let m2 = BaseMessage::human("hello");
    assert_ne!(m1.id(), m2.id(), "两条消息 id 应不同");

    // 序列化/反序列化后 id 保持一致
    let json = serde_json::to_string(&m1).unwrap();
    let restored: BaseMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.id(), m1.id(), "反序列化后 id 应保持不变");
}

#[test]
fn test_tool_call_id_persistence() {
    // 模拟完整的工具调用流程：
    // 1. AI 消息包含 tool_calls（id=toolu_123）
    // 2. Tool 消息的 tool_call_id 也是 toolu_123
    use crate::messages::ContentBlock;
    let blocks = vec![
        ContentBlock::text("I'll read a file"),
        ContentBlock::tool_use("toolu_123", "Read", serde_json::json!({"path": "test.txt"})),
    ];
    let ai_msg = BaseMessage::ai_from_blocks(blocks);

    // 验证 AI 消息包含 tool_calls
    let tcs = ai_msg.tool_calls();
    assert_eq!(tcs.len(), 1);
    assert_eq!(tcs[0].id, "toolu_123");
    assert_eq!(tcs[0].name, "Read");

    // 序列化
    let json = serde_json::to_string(&ai_msg).unwrap();

    // 反序列化
    let restored: BaseMessage = serde_json::from_str(&json).unwrap();

    // 验证 tool_calls 仍然存在
    let tcs = restored.tool_calls();
    assert_eq!(tcs.len(), 1, "反序列化后 tool_calls 应该保留");
    assert_eq!(tcs[0].id, "toolu_123");

    // 模拟 Tool 消息
    let tool_msg = BaseMessage::tool_result("toolu_123", "file content");
    let tool_json = serde_json::to_string(&tool_msg).unwrap();
    let restored_tool: BaseMessage = serde_json::from_str(&tool_json).unwrap();

    if let BaseMessage::Tool { tool_call_id, .. } = restored_tool {
        assert_eq!(tool_call_id, "toolu_123");
    } else {
        unreachable!("Tool 消息反序列化失败");
    }
}
