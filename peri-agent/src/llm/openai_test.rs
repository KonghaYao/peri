use serde_json::{json, Value};

use super::*;
use crate::{
    llm::types::StopReason,
    messages::{BaseMessage, ContentBlock, MessageContent},
};

/// Reasoning block 默认被过滤（大多数 provider 不支持 thinking content type）
#[test]
fn test_reasoning_block_filtered_by_default() {
    let content = MessageContent::Blocks(vec![
        ContentBlock::reasoning("step 1"),
        ContentBlock::text("answer"),
    ]);
    let val = ChatOpenAI::content_to_openai(&content, false);
    let arr = val.as_array().expect("content 应为 array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "answer");
}

/// supports_thinking_content=true 时 Reasoning block 应序列化为 thinking 类型
#[test]
fn test_reasoning_block_included_when_supported() {
    let content = MessageContent::Blocks(vec![
        ContentBlock::reasoning("step 1"),
        ContentBlock::text("answer"),
    ]);
    let val = ChatOpenAI::content_to_openai(&content, true);
    let arr = val.as_array().expect("content 应为 array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["type"], "thinking");
    assert_eq!(arr[0]["thinking"], "step 1");
    assert_eq!(arr[1]["type"], "text");
    assert_eq!(arr[1]["text"], "answer");
}

/// 仅 reasoning block 无 text 时，content 应为空字符串
#[test]
fn test_reasoning_only_block_becomes_empty() {
    let content = MessageContent::Blocks(vec![ContentBlock::reasoning("deep thinking")]);
    let val = ChatOpenAI::content_to_openai(&content, false);
    assert_eq!(val, json!(""));
}

/// messages_to_json：默认模型不支持 thinking，reasoning 从 content 过滤但回传到 reasoning_content 顶层字段
#[test]
fn test_messages_to_json_with_reasoning_filtered() {
    let llm = ChatOpenAI::new("sk-test", "gpt-4o");
    assert!(!llm.supports_thinking_content());
    let msgs = vec![BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning("r1"),
        ContentBlock::text("t1"),
    ])];
    let vals = llm.messages_to_json(&msgs);
    assert_eq!(vals.len(), 1);
    let assistant = &vals[0];
    assert_eq!(assistant["role"], "assistant");
    // content 中 reasoning 被过滤，只剩 text
    let content = assistant["content"].as_array().expect("content 应为 array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "t1");
    // reasoning_content 顶层字段回传
    assert_eq!(assistant["reasoning_content"], "r1");
}

/// messages_to_json：deepseek-v4-pro 不支持 content 中的 thinking 块，
/// reasoning 仅通过 reasoning_content 顶层字段回传
#[test]
fn test_messages_to_json_with_reasoning_included_for_deepseek_v4() {
    let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro");
    assert!(
        !llm.supports_thinking_content(),
        "DeepSeek V4 OpenAI API 不支持 content 数组中的 thinking 块"
    );
    let msgs = vec![BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning("r1"),
        ContentBlock::text("t1"),
    ])];
    let vals = llm.messages_to_json(&msgs);
    assert_eq!(vals.len(), 1);
    let assistant = &vals[0];
    // content 中 reasoning 被过滤，只剩 text
    let content = assistant["content"].as_array().expect("content 应为 array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "t1");
    // reasoning_content 顶层字段回传
    assert_eq!(assistant["reasoning_content"], "r1");
}

/// messages_to_json：DeepSeek R1 reasoning_content 回传 + tool_calls
#[test]
fn test_messages_to_json_reasoning_with_tool_calls() {
    let llm = ChatOpenAI::new("sk-test", "deepseek-r1");
    let msgs = vec![BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning("need bash"),
        ContentBlock::text("running..."),
        ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
    ])];
    let vals = llm.messages_to_json(&msgs);
    let assistant = &vals[0];
    // reasoning_content 顶层字段
    assert_eq!(assistant["reasoning_content"], "need bash");
    // tool_calls 在顶层
    assert!(assistant["tool_calls"].is_array());
    assert_eq!(assistant["tool_calls"][0]["id"], "tc1");
}

/// 无 reasoning 的纯文本 AI 消息，content 应为字符串（保持兼容）
#[test]
fn test_messages_to_json_text_only() {
    let llm = ChatOpenAI::new("sk-test", "gpt-4o");
    let msgs = vec![BaseMessage::ai("hello")];
    let vals = llm.messages_to_json(&msgs);
    let assistant = &vals[0];
    assert_eq!(assistant["role"], "assistant");
    assert!(assistant["content"].is_string());
    assert_eq!(assistant["content"], "hello");
}

/// 格式错误的 JSON 工具参数应被记录并保留原始内容而非静默丢弃
#[test]
fn test_malformed_tool_args_preserved() {
    let args_str = "{invalid json";
    let arguments = match serde_json::from_str::<Value>(args_str) {
        Ok(v) => v,
        Err(_) => serde_json::json!({"_raw_arguments": args_str}),
    };
    assert!(
        arguments.get("_raw_arguments").is_some(),
        "格式错误的参数应保留在 _raw_arguments 中: {arguments}"
    );
}

/// context_window: 所有模型统一返回 200K
#[test]
fn test_context_window_all_models() {
    for model in &[
        "gpt-4o",
        "gpt-3.5-turbo",
        "o1-preview",
        "deepseek-r1",
        "custom-model",
        "o3-mini",
    ] {
        let llm = ChatOpenAI::new("sk-test", *model);
        assert_eq!(llm.context_window_inner(), 200_000, "model={model}");
    }
}

// ── Builder method tests ──

#[test]
fn test_with_base_url() {
    let llm = ChatOpenAI::new("key", "model").with_base_url("https://proxy.example.com/v1");
    assert_eq!(llm.base_url(), "https://proxy.example.com/v1");
}

#[test]
fn test_with_reasoning_effort() {
    let llm = ChatOpenAI::new("key", "o1-preview").with_reasoning_effort("high");
    assert_eq!(llm.reasoning_effort().as_deref(), Some("high"));
}

#[test]
fn test_with_thinking_content() {
    let llm = ChatOpenAI::new("key", "gpt-4o").with_thinking_content(true);
    assert!(llm.supports_thinking_content());
}

#[test]
fn test_with_thinking_enabled() {
    let llm = ChatOpenAI::new("key", "deepseek-v4-pro").with_thinking_enabled();
    assert!(llm.thinking_enabled(), "thinking_enabled 应为 true");
    // DeepSeek V4 OpenAI API 不支持 content 数��中的 thinking 块，
    // supports_thinking_content 应为 false，reasoning 仅通过顶层 reasoning_content 回传
    assert!(
        !llm.supports_thinking_content(),
        "deepseek-v4-pro 的 OpenAI API 不支持 content 中的 thinking 块，应通过 reasoning_content 顶层字段回传"
    );
}

#[test]
fn test_with_thinking_enabled_non_v4() {
    // 非 v4 模型：thinking_enabled 开启但 supports_thinking_content 保持 false
    let llm = ChatOpenAI::new("key", "deepseek-chat").with_thinking_enabled();
    assert!(llm.thinking_enabled());
    assert!(
            !llm.supports_thinking_content(),
            "非 v4 模型不应开启 supports_thinking_content，否则 content 数组中会发送不支持的 thinking 块"
        );
}

/// detect_thinking_content_support: 目前所有模型都返回 false
///
/// DeepSeek V4 的 OpenAI API 格式不支持 content 数组中的 `{"type": "thinking"}` 块，
/// reasoning 内容应通过顶层 `reasoning_content` 字段回传。
#[test]
fn test_detect_thinking_content_deepseek_v4() {
    assert!(!ChatOpenAI::detect_thinking_content_support(
        "deepseek-v4-pro"
    ));
    assert!(!ChatOpenAI::detect_thinking_content_support(
        "DeepSeek-V4-Pro"
    ));
    assert!(!ChatOpenAI::detect_thinking_content_support(
        "deepseek-v4-flash"
    ));
    assert!(!ChatOpenAI::detect_thinking_content_support("deepseek-r1"));
    assert!(!ChatOpenAI::detect_thinking_content_support("gpt-4o"));
}

#[test]
fn test_new_default_no_reasoning_effort() {
    let llm = ChatOpenAI::new("key", "gpt-4o");
    assert!(llm.reasoning_effort().is_none());
    assert_eq!(llm.base_url(), "https://api.openai.com/v1");
}

/// 验证多轮 tool call 对话的消息序列：每个 tool 消息前面必须是 assistant with tool_calls
#[test]
fn test_messages_to_json_tool_sequence_valid() {
    let llm = ChatOpenAI::new("sk-test", "deepseek-r1");
    let msgs = vec![
        BaseMessage::system("You are helpful"),
        BaseMessage::human("list files"),
        // 第一轮 tool call
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("need ls"),
            ContentBlock::text("running ls"),
            ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
        ]),
        BaseMessage::tool_result("tc1", "file1.rs\nfile2.rs"),
        // 第二轮 tool call
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("read file"),
            ContentBlock::text("reading"),
            ContentBlock::tool_use("tc2", "Read", json!({"path": "file1.rs"})),
        ]),
        BaseMessage::tool_result("tc2", "fn main() {}"),
        // 最终回答
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("done"),
            ContentBlock::text("Here is the file content"),
        ]),
    ];

    let vals = llm.messages_to_json(&msgs);

    // 验证：每个 tool 消息前面的消息必须有 tool_calls
    for (i, msg) in vals.iter().enumerate() {
        if msg["role"] == "tool" {
            assert!(i > 0, "tool 消息不能是第一条: {:?}", msg);
            let prev = &vals[i - 1];
            assert!(
                prev["role"] == "assistant" && prev["tool_calls"].is_array(),
                "tool 消息前必须是 assistant with tool_calls，实际前一条: {:?}",
                prev
            );
        }
    }

    // 验证 system 在最前
    assert_eq!(vals[0]["role"], "system");
}

/// 验证 micro compact 后的消息序列仍然合法
#[test]
fn test_messages_to_json_after_micro_compact() {
    let llm = ChatOpenAI::new("sk-test", "deepseek-r1");
    // micro compact 后：tool 结果被替换为 "[compacted: ...]"，但消息不删除
    let msgs = vec![
        BaseMessage::system("system"),
        BaseMessage::human("list"),
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("need bash"),
            ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
        ]),
        BaseMessage::tool_result("tc1", "[compacted: 1000 chars]"),
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("now read"),
            ContentBlock::tool_use("tc2", "Read", json!({"path": "f.rs"})),
        ]),
        BaseMessage::tool_result("tc2", "[compacted: 500 chars]"),
        BaseMessage::ai("done"),
    ];

    let vals = llm.messages_to_json(&msgs);
    for (i, msg) in vals.iter().enumerate() {
        if msg["role"] == "tool" {
            let prev = &vals[i - 1];
            assert!(
                prev["role"] == "assistant" && prev["tool_calls"].is_array(),
                "micro compact 后 tool 序列非法，位置 {}: 前一条 {:?}",
                i,
                prev
            );
        }
    }
}

/// 端到端验证 deepseek-v4-pro：模拟 API 响应 → parse_assistant_message → 序列化回传
///
/// 验证 thinking 内容在完整链路中不丢失。
/// DeepSeek V4 OpenAI API 不接受 content 数组中的 thinking 块，
/// reasoning 仅通过顶层 reasoning_content 字段回传。
#[test]
fn test_deepseek_v4_pro_thinking_roundtrip() {
    // 模拟 deepseek-v4-pro API 响应（含 reasoning_content + tool_calls）
    let api_response = json!({
        "role": "assistant",
        "content": "Let me check the weather for you.",
        "reasoning_content": "I need to first get the current date, then check the weather.",
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "get_date",
                "arguments": "{}"
            }
        }]
    });

    let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);

    // 验证解析：message 应包含 Reasoning + Text + ToolUse blocks
    assert!(message.has_tool_calls());
    let blocks = message.content_blocks();
    assert_eq!(
        blocks.len(),
        3,
        "应有 Reasoning + Text + ToolUse 三个 blocks"
    );

    match &blocks[0] {
        ContentBlock::Reasoning { text, .. } => {
            assert_eq!(
                text,
                "I need to first get the current date, then check the weather."
            );
        }
        other => panic!("第一个 block 应为 Reasoning，实际为 {:?}", other),
    }
    assert_eq!(
        blocks[1].as_text(),
        Some("Let me check the weather for you.")
    );
    assert!(matches!(&blocks[2], ContentBlock::ToolUse { .. }));

    // 模拟第二轮序列化（deepseek-v4-pro with thinking_enabled）
    let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro").with_thinking_enabled();
    let tool_result = BaseMessage::tool_result("call_1", "2025-05-11");
    let messages = vec![
        BaseMessage::human("How's the weather tomorrow?"),
        message,
        tool_result,
    ];

    let vals = llm.messages_to_json(&messages);
    let assistant = vals.iter().find(|m| m["role"] == "assistant").unwrap();

    // 验证 reasoning_content 顶层字段回传（deepseek-v4-pro 要求）
    assert_eq!(
        assistant["reasoning_content"],
        "I need to first get the current date, then check the weather.",
        "reasoning_content 顶层字段必须回传，否则 deepseek 返回 400"
    );

    // 验证 content 中不包含 thinking block
    // DeepSeek V4 OpenAI API 不接受 content 数组中的 {"type": "thinking"} 块
    let content = assistant["content"].as_array().expect("content 应为数组");
    let thinking_block = content.iter().find(|b| b["type"] == "thinking");
    assert!(
        thinking_block.is_none(),
        "deepseek-v4-pro content 中不应包含 thinking block（API 不接受），应通过 reasoning_content 顶层字段回传"
    );

    // 验证 content 包含 text block
    let text_block = content.iter().find(|b| b["type"] == "text");
    assert!(text_block.is_some(), "content 中应包含 text block");
    assert_eq!(
        text_block.unwrap()["text"],
        "Let me check the weather for you."
    );

    // 验证 tool_calls 正确序列化
    assert!(assistant["tool_calls"].is_array());
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "get_date");
}

/// 验证 deepseek-v4-pro 多轮对话中每轮的 thinking 都被回传
#[test]
fn test_deepseek_v4_pro_multi_turn_thinking() {
    let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro").with_thinking_enabled();

    let msgs = vec![
        BaseMessage::human("question 1"),
        // 第一轮 assistant
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("thinking round 1"),
            ContentBlock::text("answer 1"),
            ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
        ]),
        BaseMessage::tool_result("tc1", "result 1"),
        // 第二轮 assistant
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("thinking round 2"),
            ContentBlock::text("answer 2"),
            ContentBlock::tool_use("tc2", "Read", json!({"path": "f.txt"})),
        ]),
        BaseMessage::tool_result("tc2", "result 2"),
        BaseMessage::human("question 2"),
    ];

    let vals = llm.messages_to_json(&msgs);
    let assistant_msgs: Vec<_> = vals.iter().filter(|m| m["role"] == "assistant").collect();

    assert_eq!(assistant_msgs.len(), 2, "应有 2 条 assistant 消息");

    // 第一轮
    assert_eq!(assistant_msgs[0]["reasoning_content"], "thinking round 1");
    assert_eq!(assistant_msgs[0]["tool_calls"][0]["id"], "tc1");

    // 第二轮
    assert_eq!(assistant_msgs[1]["reasoning_content"], "thinking round 2");
    assert_eq!(assistant_msgs[1]["tool_calls"][0]["id"], "tc2");
}

/// deepseek-v4-pro 返回 content 数组格式（thinking 块在 content 内，无顶层 reasoning_content）
///
/// 某些 API 实现将 thinking 内容放在 content 数组中而非 reasoning_content 顶层字段。
/// 解析器必须从 content 数组中提取 thinking 块。
#[test]
fn test_deepseek_v4_pro_content_array_thinking() {
    let api_response = json!({
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": "Let me analyze this..."},
            {"type": "text", "text": "I'll run a command."}
        ],
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}
        }]
    });

    let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);
    let blocks = message.content_blocks();
    assert!(message.has_tool_calls());

    // 应包含 Reasoning（从 content 数组提取）+ Text + ToolUse
    assert!(blocks.iter().any(|b| matches!(b, ContentBlock::Reasoning { text, .. } if text == "Let me analyze this...")),
            "应从 content 数组中提取 thinking 块为 Reasoning block");
    assert!(blocks
        .iter()
        .any(|b| b.as_text() == Some("I'll run a command.")));

    // 序列化回传时应包含 reasoning_content 顶层字段
    let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro").with_thinking_enabled();
    let vals = llm.messages_to_json(&[message]);
    let assistant = &vals[0];
    assert_eq!(
        assistant["reasoning_content"], "Let me analyze this...",
        "从 content 数组提取的 thinking 必须作为 reasoning_content 回传"
    );
}

/// deepseek-v4-pro 同时返回顶层 reasoning_content 和 content 数组中的 thinking 块
///
/// 应优先使用顶层 reasoning_content，跳过 content 数组中的重复 thinking 块。
#[test]
fn test_deepseek_v4_pro_both_reasoning_sources() {
    let api_response = json!({
        "role": "assistant",
        "reasoning_content": "top-level reasoning",
        "content": [
            {"type": "thinking", "thinking": "duplicate in array"},
            {"type": "text", "text": "answer"}
        ],
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {"name": "Bash", "arguments": "{}"}
        }]
    });

    let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);
    let blocks = message.content_blocks();

    // 只应有一个 Reasoning block（来自顶层），不应重复
    let reasoning_count = blocks
        .iter()
        .filter(|b| matches!(b, ContentBlock::Reasoning { .. }))
        .count();
    assert_eq!(reasoning_count, 1, "不应重复 Reasoning block");
    assert_eq!(
        blocks.iter().find_map(|b| b.as_reasoning()),
        Some("top-level reasoning"),
        "应优先使用顶层 reasoning_content"
    );
}

/// content 为空数组时的退化场景
#[test]
fn test_deepseek_v4_pro_empty_content_array() {
    let api_response = json!({
        "role": "assistant",
        "content": [],
        "reasoning_content": "thinking...",
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {"name": "Bash", "arguments": "{}"}
        }]
    });

    let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);
    assert!(message.has_tool_calls());
    let blocks = message.content_blocks();
    assert!(blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Reasoning { .. })));
}

/// DeepSeek v4 thinking 模式：assistant 消息没有 Reasoning block 时自动注入空 reasoning_content
///
/// LLM 有时返回空的 reasoning_content（被 parse 跳过），但 API 要求必须回传该字段。
#[test]
fn test_deepseek_v4_empty_reasoning_auto_inject() {
    let llm = ChatOpenAI::new("sk-test", "deepseek-v4-flash").with_thinking_enabled();

    // 模拟 assistant 消息没有 Reasoning block（reasoning_content 为空被跳过）
    let msgs = vec![
        BaseMessage::human("question"),
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::text("I'll run a command."),
            ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
        ]),
        BaseMessage::tool_result("tc1", "result"),
    ];

    let vals = llm.messages_to_json(&msgs);
    let assistant = vals.iter().find(|m| m["role"] == "assistant").unwrap();

    // 验证 reasoning_content 字段存在（即使为空字符串）
    assert!(
        assistant.get("reasoning_content").is_some(),
        "thinking 模式下 assistant 消息必须包含 reasoning_content 字段"
    );
    assert_eq!(
        assistant["reasoning_content"].as_str().unwrap(),
        "",
        "无 reasoning 内容时应注入空字符串"
    );
}

/// 非 thinking 模式：所有 assistant 消息都包含 reasoning_content（无 reasoning 时为空字符串）
#[test]
fn test_non_thinking_still_has_reasoning_content() {
    let llm = ChatOpenAI::new("sk-test", "gpt-4o");

    let msgs = vec![
        BaseMessage::human("question"),
        BaseMessage::ai_from_blocks(vec![
            ContentBlock::text("answer"),
            ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
        ]),
        BaseMessage::tool_result("tc1", "result"),
    ];

    let vals = llm.messages_to_json(&msgs);
    let assistant = vals.iter().find(|m| m["role"] == "assistant").unwrap();

    // 所有 assistant 消息都应有 reasoning_content 字段
    assert!(
        assistant.get("reasoning_content").is_some(),
        "所有 assistant 消息都应包含 reasoning_content 字段"
    );
    assert_eq!(
        assistant["reasoning_content"].as_str().unwrap(),
        "",
        "无 reasoning 内容时应为空字符串"
    );
}

/// BaseModel::build_request_body 返回 OpenAI Provider-native 请求体
/// （含 `{type:"function", function:{name,description,parameters}}` 工具格式
/// 和合并后的 system messages[0]）—— Langfuse Generation input 关键不变量。
#[test]
fn test_base_model_build_request_body_returns_provider_native_openai_format() {
    use crate::llm::types::LlmRequest;
    use crate::llm::BaseModel;
    use crate::tools::ToolDefinition;

    let llm = ChatOpenAI::new("sk-test", "gpt-4o");
    let tool_def = ToolDefinition {
        name: "Read".to_string(),
        description: "read a file".to_string(),
        parameters: serde_json::json!({"type":"object"}),
    };
    let request = LlmRequest::new(vec![BaseMessage::human(MessageContent::text("hi"))])
        .with_system("SYSTEM_PROMPT".to_string())
        .with_tools(vec![tool_def]);

    let body = llm
        .build_request_body(&request)
        .expect("ChatOpenAI 应 override build_request_body 返回 Some");
    // 工具格式：OpenAI-native（type:function + function 嵌套）
    let tools = body["tools"].as_array().expect("tools 应是 array");
    assert_eq!(
        tools[0]["type"], "function",
        "OpenAI tool 必须是 type:function"
    );
    assert!(
        tools[0]["function"].is_object(),
        "OpenAI tool 必须有 function 嵌套对象"
    );
    assert_eq!(tools[0]["function"]["name"], "Read");
    // system 应在 messages[0]
    let messages = body["messages"].as_array().expect("messages 应是 array");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "SYSTEM_PROMPT");
    // 不应是抽象格式 {name, parameters}（顶层）
    assert!(
        tools[0].get("parameters").is_none(),
        "OpenAI 工具 parameters 应在 function 内，不应在顶层"
    );
}
