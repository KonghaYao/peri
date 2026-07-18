use serde_json::json;

use super::{cache, *};
use crate::messages::{BaseMessage, ContentBlock, MessageContent};

/// 验证 cache_control 放在第一条和最后一条 user 消息上（3 断点策略）
#[test]
fn test_cache_control_on_first_and_last_user_messages() {
    let mut messages = vec![
        json!({"role": "user", "content": "first question"}),
        json!({"role": "assistant", "content": "first answer"}),
        json!({"role": "user", "content": "second question"}),
    ];
    cache::apply_cache_to_messages(&mut messages);

    // 第一条 user 消息（index 0）应被转换为 blocks 并包含 cache_control
    let content = messages[0]["content"].as_array().unwrap();
    let first_block = &content[0];
    assert_eq!(
        first_block["cache_control"]["type"], "ephemeral",
        "第一条 user 消息应有 cache_control"
    );
    assert_eq!(first_block["text"], "first question");

    // 第二条 user 消息（index 2）也应有 cache_control（3 断点策略：最后一条）
    let content2 = messages[2]["content"].as_array().unwrap();
    assert_eq!(
        content2[0]["cache_control"]["type"], "ephemeral",
        "最后一条 user 消息应有 cache_control"
    );
}

/// 验证 3 条及以上 user 消息时，倒数第二条也加断点
#[test]
fn test_cache_control_three_user_messages_gets_second_to_last() {
    let mut messages = vec![
        json!({"role": "user", "content": "q1"}),
        json!({"role": "assistant", "content": "a1"}),
        json!({"role": "user", "content": "q2"}),
        json!({"role": "assistant", "content": "a2"}),
        json!({"role": "user", "content": "q3"}),
    ];
    cache::apply_cache_to_messages(&mut messages);

    // index 0 (q1): 第一条 → 有断点
    assert_eq!(
        messages[0]["content"].as_array().unwrap()[0]["cache_control"]["type"],
        "ephemeral"
    );
    // index 2 (q2): 倒数第二条 → 有断点
    assert_eq!(
        messages[2]["content"].as_array().unwrap()[0]["cache_control"]["type"],
        "ephemeral"
    );
    // index 4 (q3): 最后一条 → 有断点
    assert_eq!(
        messages[4]["content"].as_array().unwrap()[0]["cache_control"]["type"],
        "ephemeral"
    );
}

/// 验证 assistant 消息被跳过，从不设置 cache_control
#[test]
fn test_cache_control_skips_assistant() {
    let mut messages = vec![
        json!({"role": "assistant", "content": "assistant only"}),
        json!({"role": "user", "content": "first user"}),
    ];
    cache::apply_cache_to_messages(&mut messages);

    // assistant 消息应不变（index 0）
    assert!(messages[0]["content"].is_string());
    // 第一条 user 消息（index 1）应被转换
    let content = messages[1]["content"].as_array().unwrap();
    assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
}

/// 验证多 block 消息：cache_control 加在最后一个非空 text block 上
#[test]
fn test_cache_control_on_last_text_block() {
    let mut messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "block 1"},
            {"type": "text", "text": "block 2"},
        ]
    })];
    cache::apply_cache_to_messages(&mut messages);

    let blocks = messages[0]["content"].as_array().unwrap();
    assert!(!blocks[0].as_object().unwrap().contains_key("cache_control"));
    assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
}

/// H3 修复：断点跳过尾部的 tool_result，落在 text block 上
#[test]
fn test_cache_control_skips_trailing_tool_result() {
    let mut messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "question"},
            {"type": "tool_result", "tool_use_id": "t1", "content": "result data"}
        ]
    })];
    cache::apply_cache_to_messages(&mut messages);
    let blocks = messages[0]["content"].as_array().unwrap();
    assert_eq!(
        blocks[0]["cache_control"]["type"], "ephemeral",
        "断点应在 text block"
    );
    assert!(
        !blocks[1].as_object().unwrap().contains_key("cache_control"),
        "tool_result 不应有断点"
    );
}

/// H3 修复：尾部有 tool_use + tool_result 时，断点跳过两者
#[test]
fn test_cache_control_skips_tool_use_and_tool_result() {
    let mut messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "question"},
            {"type": "tool_use", "id": "t1", "name": "Read", "input": {}},
            {"type": "tool_result", "tool_use_id": "t1", "content": "file contents"}
        ]
    })];
    cache::apply_cache_to_messages(&mut messages);
    let blocks = messages[0]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    assert!(!blocks[1].as_object().unwrap().contains_key("cache_control"));
    assert!(!blocks[2].as_object().unwrap().contains_key("cache_control"));
}

/// H3 修复：全 tool block 时跳过该断点
#[test]
fn test_cache_control_skips_all_tool_blocks() {
    let mut messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "data1"},
            {"type": "tool_result", "tool_use_id": "t2", "content": "data2"}
        ]
    })];
    cache::apply_cache_to_messages(&mut messages);
    let blocks = messages[0]["content"].as_array().unwrap();
    for b in blocks {
        assert!(
            !b.as_object().unwrap().contains_key("cache_control"),
            "tool block 不应有断点"
        );
    }
}

/// 验证空 text block 被跳过
#[test]
fn test_cache_control_skips_empty_text_block() {
    let mut messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": ""},
            {"type": "text", "text": "real content"},
        ]
    })];
    cache::apply_cache_to_messages(&mut messages);

    let blocks = messages[0]["content"].as_array().unwrap();
    // 空 block 无 cache_control
    assert!(!blocks[0].as_object().unwrap().contains_key("cache_control"));
    // 非空 block 有 cache_control
    assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
}

/// 验证无 user 消息时不变更
#[test]
fn test_cache_control_no_user_messages() {
    let mut messages = vec![json!({"role": "assistant", "content": "only assistant"})];
    let before = messages.clone();
    cache::apply_cache_to_messages(&mut messages);
    assert_eq!(messages, before, "无 user 消息时应不变");
}

/// 回退搜索：second-to-last 为 tool_result-only 时，回退到更早的 user message
#[test]
fn test_cache_control_fallback_second_to_last_tool_result() {
    let mut messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "first question"}]}),
        json!({"role": "assistant", "content": "answer"}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "data"}]}),
        json!({"role": "assistant", "content": "more"}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2", "content": "data2"}]}),
    ];
    cache::apply_cache_to_messages(&mut messages);
    // first user (index 0) → 有断点
    assert_eq!(
        messages[0]["content"].as_array().unwrap()[0]["cache_control"]["type"],
        "ephemeral"
    );
    // last user (index 4, tool_result-only) → 回退到 index 0（已去重，不再添加）
    // second-to-last (index 2, tool_result-only) → 回退到 index 0（已去重，不再添加）
    // 所以只有 index 0 有断点
    assert!(
        !messages[2]["content"].as_array().unwrap()[0]
            .as_object()
            .unwrap()
            .contains_key("cache_control"),
        "tool_result-only 消息不应有断点"
    );
    assert!(
        !messages[4]["content"].as_array().unwrap()[0]
            .as_object()
            .unwrap()
            .contains_key("cache_control"),
        "tool_result-only 消息不应有断点"
    );
}

/// 回退搜索：second-to-last 为 tool_result-only，但有更早的含 text user message
#[test]
fn test_cache_control_fallback_finds_earlier_text_user() {
    let mut messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "question 1"}]}),
        json!({"role": "assistant", "content": "answer 1"}),
        json!({"role": "user", "content": [{"type": "text", "text": "question 2"}]}),
        json!({"role": "assistant", "content": "answer 2"}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "data"}]}),
        json!({"role": "assistant", "content": "answer 3"}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2", "content": "data2"}]}),
    ];
    cache::apply_cache_to_messages(&mut messages);
    // first (index 0) → 有断点
    assert_eq!(
        messages[0]["content"].as_array().unwrap()[0]["cache_control"]["type"],
        "ephemeral"
    );
    // second-to-last (index 5→实际 user index 5 不存在, user indices = [0,2,4,6])
    // second-to-last user = index 4 (tool_result-only) → 回退到 index 2 (含 text)
    assert_eq!(
        messages[2]["content"].as_array().unwrap()[0]["cache_control"]["type"],
        "ephemeral",
        "回退应找到 index 2 的 text block"
    );
    // last user (index 6, tool_result-only) → 回退搜索，但 index 0 和 2 已被占用
    // 不会再添加新的断点
}

// ── Builder method tests ──

#[test]
fn test_with_base_url() {
    let llm = ChatAnthropic::new("key", "model").with_base_url("https://proxy.example.com");
    assert_eq!(llm.base_url().as_deref(), Some("https://proxy.example.com"));
}

#[test]
fn test_with_base_url_empty_is_none() {
    let llm = ChatAnthropic::new("key", "model").with_base_url("");
    assert!(llm.base_url().is_none());
}

#[test]
fn test_with_extended_thinking_passes_through_budget() {
    let llm = ChatAnthropic::new("key", "model").with_extended_thinking(100, "high");
    assert!(llm.extended_thinking());
    assert_eq!(
        llm.thinking_budget(),
        100,
        "budget_tokens 应原样传递，不做截断"
    );
    assert_eq!(llm.thinking_effort(), "high");
}

#[test]
fn test_with_extended_thinking_valid_budget() {
    let llm = ChatAnthropic::new("key", "model").with_extended_thinking(5000, "low");
    assert_eq!(llm.thinking_budget(), 5000);
}

#[test]
fn test_without_cache() {
    let llm = ChatAnthropic::new("key", "model").without_cache();
    assert!(!llm.enable_cache());
}

// ── split_system_blocks 测试 ─────────────────────────────────────────

#[test]
fn test_split_system_blocks_with_boundary() {
    let text = "static content\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n\ndynamic content";
    let blocks = cache::split_system_blocks(text);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].text, "static content");
    assert!(blocks[0].cache_control);
    assert_eq!(blocks[1].text, "dynamic content");
    assert!(!blocks[1].cache_control);
}

#[test]
fn test_split_system_blocks_without_boundary() {
    let text = "no boundary here";
    let blocks = cache::split_system_blocks(text);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "no boundary here");
    assert!(!blocks[0].cache_control);
}

#[test]
fn test_split_system_blocks_empty() {
    let blocks = cache::split_system_blocks("");
    assert!(blocks.is_empty());
}

#[test]
fn test_split_system_blocks_empty_static_part() {
    let text = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n\ndynamic only";
    let blocks = cache::split_system_blocks(text);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "dynamic only");
    assert!(!blocks[0].cache_control);
}

#[test]
fn test_split_system_blocks_empty_dynamic_part() {
    let text = "static only\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n\n";
    let blocks = cache::split_system_blocks(text);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "static only");
    assert!(blocks[0].cache_control);
}

#[test]
fn test_split_system_blocks_multiple_sections() {
    let text = "core rules\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n\ndate: 2026-05-13\n\ncwd: /tmp\n\nmiddleware content";
    let blocks = cache::split_system_blocks(text);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].text, "core rules");
    assert!(blocks[0].cache_control);
    assert!(blocks[1].text.contains("date: 2026-05-13"));
    assert!(blocks[1].text.contains("middleware content"));
    assert!(!blocks[1].cache_control);
}

#[test]
fn test_messages_to_anthropic_system_blocks() {
    let messages = vec![
        BaseMessage::system("static\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n\ndynamic"),
        BaseMessage::human("hello"),
    ];
    let (msgs, blocks) = ChatAnthropic::messages_to_anthropic(&messages);
    assert_eq!(msgs.len(), 1); // 只有 user 消息
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].cache_control);
    assert!(!blocks[1].cache_control);
}

#[test]
fn test_messages_to_anthropic_no_boundary() {
    let messages = vec![
        BaseMessage::system("plain system prompt"),
        BaseMessage::human("hello"),
    ];
    let (_msgs, blocks) = ChatAnthropic::messages_to_anthropic(&messages);
    assert_eq!(blocks.len(), 1);
    assert!(!blocks[0].cache_control);
}

/// middleware 注入的 System 消息（无边界标记）应放在边界之后，
/// 不破坏缓存前缀
#[test]
fn test_messages_to_anthropic_middleware_after_boundary() {
    // 模拟 prepend_message 后的消息顺序：
    // [system_prompt(含边界), tool_search_system, agents_md_system, Human]
    let messages = vec![
        BaseMessage::system("static\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n\ndynamic"),
        BaseMessage::system("tool search index content"),
        BaseMessage::system("CLAUDE.md content here"),
        BaseMessage::human("hello"),
    ];
    let (msgs, blocks) = ChatAnthropic::messages_to_anthropic(&messages);
    assert_eq!(msgs.len(), 1, "只有 user 消息");
    assert_eq!(blocks.len(), 2, "应拆分为静态块和动态块");
    // 静态块不含 middleware 内容
    assert!(blocks[0].cache_control);
    assert!(!blocks[0].text.contains("tool search index"));
    assert!(!blocks[0].text.contains("CLAUDE.md"));
    // 动态块包含 middleware 内容
    assert!(!blocks[1].cache_control);
    assert!(blocks[1].text.contains("tool search index"));
    assert!(blocks[1].text.contains("CLAUDE.md"));
}

#[test]
fn test_default_values() {
    let llm = ChatAnthropic::new("key", "claude-sonnet-4-6");
    assert!(!llm.extended_thinking());
    assert_eq!(llm.thinking_budget(), 10000);
    assert_eq!(llm.thinking_effort(), "medium");
    assert!(llm.enable_cache());
    assert!(llm.base_url().is_none());
}

/// 验证 assistant 消息含 thinking + tool_use 时，thinking blocks 被正确回传
///
/// 场景：第一轮 API 返回 [thinking, text, tool_use]，序列化写入 state，
/// 第二轮构建请求时 messages_to_anthropic 应保留 thinking block。
#[test]
fn test_messages_to_anthropic_preserves_thinking_with_tool_use() {
    // 模拟第一轮 API 响应后写入 state 的 AI 消息
    // source_message 保留完整 blocks：thinking + text + tool_use
    let ai_msg = BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning_with_signature("I need to read a file", "sig_abc123"),
        ContentBlock::text("Let me read the file for you."),
        ContentBlock::tool_use("tc_1", "Read", json!({"file_path": "/tmp/test.txt"})),
    ]);
    assert!(ai_msg.has_tool_calls());

    let tool_result = BaseMessage::tool_result("tc_1", "file contents here");

    let messages = vec![
        BaseMessage::human("read /tmp/test.txt"),
        ai_msg,
        tool_result,
    ];

    let (msgs, _system) = ChatAnthropic::messages_to_anthropic(&messages);

    // 应有 2 条消息：user(human) + user(tool_result 合并)
    // assistant 消息应在 user 消息之前
    let assistant_idx = msgs.iter().position(|m| m["role"] == "assistant");
    assert!(assistant_idx.is_some(), "应有 assistant 消息");

    let assistant = &msgs[assistant_idx.unwrap()];
    let content = assistant["content"].as_array().expect("content 应为数组");

    // 验证 thinking block 存在且在第一个位置
    assert_eq!(content[0]["type"], "thinking", "第一个 block 应为 thinking");
    assert_eq!(content[0]["thinking"], "I need to read a file");
    assert_eq!(
        content[0]["signature"], "sig_abc123",
        "thinking block 应包含 signature"
    );

    // 验证 text block
    let text_block = content.iter().find(|b| b["type"] == "text");
    assert!(text_block.is_some(), "应有 text block");
    assert_eq!(text_block.unwrap()["text"], "Let me read the file for you.");

    // 验证 tool_use block
    let tool_block = content.iter().find(|b| b["type"] == "tool_use");
    assert!(tool_block.is_some(), "应有 tool_use block");
    assert_eq!(tool_block.unwrap()["id"], "tc_1");
    assert_eq!(tool_block.unwrap()["name"], "Read");
}

/// 验证 assistant 消息只有 thinking + tool_use（无 text）时也能正确保留
#[test]
fn test_messages_to_anthropic_preserves_thinking_without_text() {
    let ai_msg = BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning_with_signature("just thinking", "sig_xyz"),
        ContentBlock::tool_use("tc_2", "Bash", json!({"command": "ls"})),
    ]);

    let messages = vec![
        BaseMessage::human("list files"),
        ai_msg,
        BaseMessage::tool_result("tc_2", "file1.txt\nfile2.txt"),
    ];

    let (msgs, _system) = ChatAnthropic::messages_to_anthropic(&messages);
    let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    let content = assistant["content"].as_array().unwrap();

    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["signature"], "sig_xyz");
    assert_eq!(content[1]["type"], "tool_use");
}

/// 验证 redacted_thinking block（ContentBlock::Unknown）也能正确透传
#[test]
fn test_messages_to_anthropic_preserves_redacted_thinking() {
    let redacted_block = json!({
        "type": "redacted_thinking",
        "data": "abc123"
    });
    let ai_msg = BaseMessage::ai_from_blocks(vec![
        ContentBlock::Unknown(redacted_block),
        ContentBlock::tool_use("tc_3", "Bash", json!({"command": "echo hi"})),
    ]);

    let messages = vec![
        BaseMessage::human("say hi"),
        ai_msg,
        BaseMessage::tool_result("tc_3", "hi"),
    ];

    let (msgs, _system) = ChatAnthropic::messages_to_anthropic(&messages);
    let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    let content = assistant["content"].as_array().unwrap();

    assert_eq!(content[0]["type"], "redacted_thinking");
    assert_eq!(content[0]["data"], "abc123");
    assert_eq!(content[1]["type"], "tool_use");
}

// ── ensure_thinking_blocks tests ──

#[test]
fn test_ensure_thinking_blocks_injects_for_missing() {
    // 模拟 SkillPreloadMiddleware 注入的伪 assistant 消息（仅有 tool_use，无 thinking）
    let mut messages = vec![
        json!({"role": "user", "content": "do something"}),
        json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "call_1", "name": "Read", "input": {"path": "/tmp/skill.md"}}
            ]
        }),
        json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "skill content"}]
        }),
    ];

    cache::ensure_thinking_blocks(&mut messages);

    // assistant 消息（index 1）应被注入 thinking
    let assistant = &messages[1];
    let content = assistant["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "应有 2 个 blocks（thinking + tool_use）");
    assert_eq!(content[0]["type"], "thinking", "第一个 block 应为 thinking");
    assert_eq!(content[0]["thinking"], "", "占位 thinking 文本为空");
    assert_eq!(content[1]["type"], "tool_use", "第二个 block 应为 tool_use");
}

#[test]
fn test_ensure_thinking_blocks_skips_when_thinking_present() {
    let mut messages = vec![json!({
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": "I'm thinking", "signature": "sig_123"},
            {"type": "tool_use", "id": "call_1", "name": "Read", "input": {}}
        ]
    })];

    cache::ensure_thinking_blocks(&mut messages);

    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "已有 thinking，不应注入额外的占位 block");
    assert_eq!(content[0]["type"], "thinking");
}

#[test]
fn test_ensure_thinking_blocks_skips_when_redacted_thinking_present() {
    let mut messages = vec![json!({
        "role": "assistant",
        "content": [
            {"type": "redacted_thinking", "data": "opaque"},
            {"type": "text", "text": "done"}
        ]
    })];

    cache::ensure_thinking_blocks(&mut messages);

    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "已有 redacted_thinking，不应再注入");
}

#[test]
fn test_ensure_thinking_blocks_ignores_non_assistant() {
    let mut messages = vec![
        json!({"role": "user", "content": "hello"}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "no thinking"}]}),
    ];

    cache::ensure_thinking_blocks(&mut messages);

    // user 消息不应被修改
    assert_eq!(messages[0]["role"], "user");
    assert!(
        messages[0]["content"].is_string(),
        "user content 应保持为字符串"
    );

    // assistant 消息应被注入 thinking
    let content = messages[1]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
}

#[test]
fn test_ensure_thinking_blocks_string_content() {
    // assistant 消息 content 为字符串（非数组）的情况
    let mut messages = vec![json!({
        "role": "assistant",
        "content": "plain text response"
    })];

    cache::ensure_thinking_blocks(&mut messages);

    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[1], "plain text response");
}

#[test]
fn test_ensure_thinking_blocks_mixed_messages() {
    // 混合场景：有 thinking 的和没有 thinking 的 assistant 消息
    let mut messages = vec![
        json!({"role": "user", "content": "q1"}),
        json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "thought", "signature": "sig"},
                {"type": "text", "text": "a1"}
            ]
        }),
        json!({"role": "user", "content": "q2"}),
        // 伪 assistant 消息（无 thinking）
        json!({
            "role": "assistant",
            "content": [{"type": "tool_use", "id": "c1", "name": "Read", "input": {}}]
        }),
        json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "c1", "content": "data"}]
        }),
    ];

    cache::ensure_thinking_blocks(&mut messages);

    // msg[1]: 已有 thinking，不应被修改
    let content1 = messages[1]["content"].as_array().unwrap();
    assert_eq!(content1.len(), 2, "已有 thinking 的消息不应增加 block");
    assert_eq!(content1[0]["type"], "thinking");

    // msg[3]: 无 thinking，应被注入
    let content3 = messages[3]["content"].as_array().unwrap();
    assert_eq!(content3.len(), 2, "应注入 thinking");
    assert_eq!(content3[0]["type"], "thinking");
    assert_eq!(content3[1]["type"], "tool_use");
}

/// 端到端验证：模拟 Anthropic API 响应 → parse_content_blocks → message 构造 → 序列化回传
///
/// 验证 thinking block 在完整链路中不丢失。
#[test]
fn test_parse_and_reserialize_thinking_with_tool_use() {
    // 模拟 Anthropic API 返回的 content 数组（extended thinking + tool_use）
    let api_response_blocks = vec![
        json!({
            "type": "thinking",
            "thinking": "I need to check the file first",
            "signature": "sig_12345"
        }),
        json!({
            "type": "text",
            "text": "Let me read that file."
        }),
        json!({
            "type": "tool_use",
            "id": "toolu_01",
            "name": "Read",
            "input": {"file_path": "/tmp/test.rs"}
        }),
    ];

    let (blocks, tool_calls) = ChatAnthropic::parse_content_blocks(&api_response_blocks);

    // 验证解析结果
    assert_eq!(blocks.len(), 3, "应解析出 3 个 blocks");
    assert_eq!(tool_calls.len(), 1, "应有 1 个 tool_call");

    // 第一个 block 应是 Reasoning
    match &blocks[0] {
        ContentBlock::Reasoning { text, signature } => {
            assert_eq!(text, "I need to check the file first");
            assert_eq!(signature.as_deref(), Some("sig_12345"));
        }
        other => panic!("第一个 block 应为 Reasoning，实际为 {:?}", other),
    }

    // 模拟 invoke() 中的 message 构造逻辑（第 542-552 行）
    let message = if !tool_calls.is_empty() {
        let content = if let [single] = blocks.as_slice() {
            if let Some(text) = single.as_text() {
                MessageContent::text(text)
            } else {
                MessageContent::Blocks(blocks)
            }
        } else {
            MessageContent::Blocks(blocks)
        };
        BaseMessage::ai_with_tool_calls(content, tool_calls)
    } else {
        unreachable!()
    };

    // 验证 message 的 content 类型
    match &message {
        BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } => {
            assert_eq!(tool_calls.len(), 1);
            assert!(
                matches!(content, MessageContent::Blocks(_)),
                "content 应为 Blocks 类型"
            );

            // 验证 content_blocks 包含 thinking
            let content_blocks = content.content_blocks();
            assert_eq!(content_blocks.len(), 3);
            assert!(matches!(&content_blocks[0], ContentBlock::Reasoning { .. }));
        }
        _ => panic!("应为 Ai 消息"),
    }

    // 模拟第二轮请求的序列化
    let tool_result = BaseMessage::tool_result("toolu_01", "fn main() {}");
    let messages = vec![BaseMessage::human("show me test.rs"), message, tool_result];

    let (msgs, _system) = ChatAnthropic::messages_to_anthropic(&messages);
    let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    let content = assistant["content"].as_array().unwrap();

    // 关键验证：thinking block 在序列化后被保留
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "I need to check the file first");
    assert_eq!(content[0]["signature"], "sig_12345");
}

/// 验证 AtMentionMiddleware/SkillPreloadMiddleware 注入的 fake Read 工具消息
/// 经过 messages_to_anthropic 后格式正确（不会导致 API 400）
#[test]
fn test_preload_fake_read_tool_messages_anthropic_format() {
    // 模拟 AtMentionMiddleware 注入后的 state.messages
    let messages = vec![
        BaseMessage::human("请看 @test.rs"),
        BaseMessage::ai_from_blocks(vec![ContentBlock::tool_use(
            "call_abc123".to_string(),
            "Read",
            json!({ "file_path": "test.rs" }),
        )]),
        BaseMessage::tool_result("call_abc123".to_string(), "fn main() {}"),
    ];

    let (msgs, _system) = ChatAnthropic::messages_to_anthropic(&messages);

    // 验证 messages 数组结构
    assert_eq!(
        msgs.len(),
        3,
        "应有 3 条消息: user + assistant + user(tool_result)"
    );

    // messages[0] = user（原始 Human 消息）
    assert_eq!(msgs[0]["role"], "user");
    let content0 = msgs[0]["content"].as_array().unwrap();
    // 不应包含 tool_result block
    assert!(
        content0.iter().all(|b| b["type"] != "tool_result"),
        "第一条消息不应包含 tool_result: {:?}",
        content0
    );

    // messages[1] = assistant（包含 tool_use）
    assert_eq!(msgs[1]["role"], "assistant");
    let content1 = msgs[1]["content"].as_array().unwrap();
    assert!(
        content1.iter().any(|b| b["type"] == "tool_use"),
        "assistant 消息应包含 tool_use"
    );

    // messages[2] = user（包含 tool_result，引用前一条 assistant 的 tool_use）
    assert_eq!(msgs[2]["role"], "user");
    let content2 = msgs[2]["content"].as_array().unwrap();
    assert!(
        content2.iter().any(|b| b["type"] == "tool_result"),
        "应有 tool_result block"
    );

    // 验证 tool_use_id 引用正确
    let tool_result = content2
        .iter()
        .find(|b| b["type"] == "tool_result")
        .unwrap();
    assert_eq!(tool_result["tool_use_id"], "call_abc123");
}

/// 验证两轮对话中 preload 消息保留在 history 时格式仍然正确
/// 模拟用户场景：第一轮 @file 有匹配 → 注入 Ai+Tool → LLM 回复 →
/// 第二轮 history 包含上一轮 preload 消息 + 新 Human → 不应 400
#[test]
fn test_preload_messages_preserved_in_second_round_history() {
    // 第一轮结束后的 history（包含 preload 注入的 fake Read）
    let round1_history = vec![
        BaseMessage::human("@test.rs"), // 用户消息
        BaseMessage::ai_from_blocks(vec![
            // AtMentionMiddleware 注入
            ContentBlock::tool_use(
                "call_round1_read".to_string(),
                "Read",
                json!({ "file_path": "test.rs" }),
            ),
        ]),
        BaseMessage::tool_result(
            // AtMentionMiddleware 注入
            "call_round1_read".to_string(),
            "fn main() {}",
        ),
        BaseMessage::ai("Here is the file content..."), // LLM 回复
    ];

    // 第二轮：history + 新 Human 消息
    let round2_messages = {
        let mut msgs = round1_history;
        msgs.push(BaseMessage::human("thanks, now help me with the code"));
        msgs
    };

    let (msgs, _system) = ChatAnthropic::messages_to_anthropic(&round2_messages);

    // 验证结构
    assert!(
        msgs.len() >= 4,
        "至少 4 条消息: user, assistant(tool_use), user(tool_result), assistant, user"
    );

    // messages[0] 必须是 user 且不含 tool_result
    assert_eq!(msgs[0]["role"], "user");
    let c0 = msgs[0]["content"].as_array().unwrap();
    assert!(
        c0.iter().all(|b| b["type"] != "tool_result"),
        "messages[0] 不应包含 tool_result: {:?}",
        c0
    );

    // messages[1] = assistant(tool_use)
    assert_eq!(msgs[1]["role"], "assistant");

    // messages[2] = user(tool_result)
    assert_eq!(msgs[2]["role"], "user");
}

/// 验证 history 以 Tool 消息开头时的错误场景（排查 messages.0 问题）
/// 如果 history 不当丢失了 Human 消息，Tool 消息会变成 messages[0]
#[test]
fn test_tool_message_as_first_in_history_is_invalid() {
    // 模拟 history 中 Human 消息丢失，Tool 消息成为第一条
    let bad_history = vec![
        BaseMessage::tool_result("call_orphan".to_string(), "orphan content"),
        BaseMessage::ai("response"),
    ];

    let (msgs, _system) = ChatAnthropic::messages_to_anthropic(&bad_history);

    // 这种情况 messages[0] 会变成 {user, [tool_result]} — 这就是 400 的根因
    assert_eq!(msgs[0]["role"], "user");
    let c0 = msgs[0]["content"].as_array().unwrap();
    // 这个测试验证了问题场景：messages[0].content[0] 是 tool_result
    assert_eq!(
        c0[0]["type"], "tool_result",
        "确认：Tool 作为第一条消息时，会被转为 user(tool_result) 作为 messages[0]"
    );
}

// ── build_system_blocks_json 测试 ───────────────────────────────────────

/// Two blocks (boundary present): static block has cache_control=true from split_system_blocks.
/// Dynamic block should NOT get cache_control — no fallback needed.
#[test]
fn test_system_blocks_cache_control_two_blocks() {
    let blocks = vec![
        cache::SystemPromptBlock {
            text: "static sections".into(),
            cache_control: true,
        },
        cache::SystemPromptBlock {
            text: "dynamic content with date/cwd/language".into(),
            cache_control: false,
        },
    ];
    let json_blocks = ChatAnthropic::build_system_blocks_json(&blocks);
    assert_eq!(json_blocks.len(), 2);
    assert_eq!(
        json_blocks[0]["cache_control"]["type"], "ephemeral",
        "Block 0 (static) must have cache_control"
    );
    assert!(
        !json_blocks[1]
            .as_object()
            .unwrap()
            .contains_key("cache_control"),
        "Block 1 (dynamic) must NOT have cache_control"
    );
}

/// Single block (no boundary): no preceding cache_control.
/// Fallback MUST apply to the last block.
#[test]
fn test_system_blocks_cache_control_single_block() {
    let blocks = vec![cache::SystemPromptBlock {
        text: "plain prompt without boundary marker".into(),
        cache_control: false,
    }];
    let json_blocks = ChatAnthropic::build_system_blocks_json(&blocks);
    assert_eq!(json_blocks.len(), 1);
    assert_eq!(
        json_blocks[0]["cache_control"]["type"], "ephemeral",
        "Single block with no preceding cache_control must get fallback"
    );
}

/// Four blocks: middleware chunks (no cache) + split output (static cached, dynamic not).
/// Only the static block should have cache_control.
#[test]
fn test_system_blocks_cache_control_middleware_blocks() {
    let blocks = vec![
        cache::SystemPromptBlock {
            text: "middleware chunk 1".into(),
            cache_control: false,
        },
        cache::SystemPromptBlock {
            text: "middleware chunk 2".into(),
            cache_control: false,
        },
        cache::SystemPromptBlock {
            text: "static sections".into(),
            cache_control: true,
        },
        cache::SystemPromptBlock {
            text: "dynamic content".into(),
            cache_control: false,
        },
    ];
    let json_blocks = ChatAnthropic::build_system_blocks_json(&blocks);
    assert_eq!(json_blocks.len(), 4);
    assert!(!json_blocks[0]
        .as_object()
        .unwrap()
        .contains_key("cache_control"));
    assert!(!json_blocks[1]
        .as_object()
        .unwrap()
        .contains_key("cache_control"));
    assert_eq!(json_blocks[2]["cache_control"]["type"], "ephemeral");
    assert!(
        !json_blocks[3]
            .as_object()
            .unwrap()
            .contains_key("cache_control"),
        "Block 3 (dynamic) must NOT get cache_control — Block 2 (static) already has it"
    );
}

/// All blocks have cache_control=false: fallback must apply to last block.
#[test]
fn test_system_blocks_cache_control_all_false_fallback() {
    let blocks = vec![
        cache::SystemPromptBlock {
            text: "chunk 1".into(),
            cache_control: false,
        },
        cache::SystemPromptBlock {
            text: "chunk 2".into(),
            cache_control: false,
        },
    ];
    let json_blocks = ChatAnthropic::build_system_blocks_json(&blocks);
    assert_eq!(json_blocks.len(), 2);
    assert!(!json_blocks[0]
        .as_object()
        .unwrap()
        .contains_key("cache_control"));
    assert_eq!(
        json_blocks[1]["cache_control"]["type"], "ephemeral",
        "Last block must get fallback since no preceding cache_control"
    );
}

/// 单块已缓存：should keep the flag (no fallback needed, fallback wouldn't add duplicate anyway).
#[test]
fn test_system_blocks_single_cached_block_no_duplicate() {
    let blocks = vec![cache::SystemPromptBlock {
        text: "x".into(),
        cache_control: true,
    }];
    let json_blocks = ChatAnthropic::build_system_blocks_json(&blocks);
    assert!(
        json_blocks[0]
            .as_object()
            .unwrap()
            .contains_key("cache_control"),
        "Single already-cached block should keep cache_control"
    );
}

/// 验证 tool_result 和 tool_error 消息序列化后都包含 id 字段（GLM 兼容性）
#[test]
fn test_tool_result_and_tool_error_both_have_id_field() {
    let messages = vec![
        BaseMessage::human("do something"),
        BaseMessage::ai_with_tool_calls(
            "",
            vec![crate::messages::ToolCallRequest::new(
                "call_1".to_string(),
                "Read".to_string(),
                json!({"file_path": "test.rs"}),
            )],
        ),
        BaseMessage::tool_result("call_1", "file content"),
        BaseMessage::ai_with_tool_calls(
            "",
            vec![crate::messages::ToolCallRequest::new(
                "call_2".to_string(),
                "Write".to_string(),
                json!({"file_path": "test.rs"}),
            )],
        ),
        BaseMessage::tool_error("call_2", "permission denied"),
    ];

    let (msgs, _) = ChatAnthropic::messages_to_anthropic(&messages);

    // 找到所有 tool_result block
    let mut tool_results: Vec<&serde_json::Value> = Vec::new();
    for msg in &msgs {
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block["type"] == "tool_result" {
                    tool_results.push(block);
                }
            }
        }
    }

    assert_eq!(
        tool_results.len(),
        2,
        "应有 2 个 tool_result block（一个 success 一个 error）"
    );

    for (i, tr) in tool_results.iter().enumerate() {
        assert!(
            tr.get("id").is_some(),
            "第 {} 个 tool_result 缺少 id 字段: {:?}",
            i,
            tr
        );
        assert!(
            !tr["id"].as_str().unwrap_or("").is_empty(),
            "第 {} 个 tool_result 的 id 为空字符串: {:?}",
            i,
            tr
        );
    }

    // 验证 is_error 标志正确
    let success_tr = tool_results
        .iter()
        .find(|t| t["tool_use_id"] == "call_1")
        .unwrap();
    assert_eq!(
        success_tr["is_error"], false,
        "tool_result 的 is_error 应为 false"
    );

    let error_tr = tool_results
        .iter()
        .find(|t| t["tool_use_id"] == "call_2")
        .unwrap();
    assert_eq!(
        error_tr["is_error"], true,
        "tool_error 的 is_error 应为 true"
    );
}

/// 端到端验证：多轮工具调用（含 tool_error）后的完整请求体中，
/// 所有 tool_result block 都包含 id 字段
#[test]
fn test_multiturn_with_errors_all_tool_results_have_id() {
    // 模拟场景：3 轮工具调用，第 2 轮和第 3 轮有 tool_error
    let messages = vec![
        BaseMessage::human("read a file"),
        BaseMessage::ai_with_tool_calls(
            "",
            vec![crate::messages::ToolCallRequest::new(
                "call_1".to_string(),
                "Read".to_string(),
                json!({"file_path": "test.rs"}),
            )],
        ),
        BaseMessage::tool_result("call_1", "fn main() {}"),
        BaseMessage::ai_with_tool_calls(
            "Let me write to it",
            vec![crate::messages::ToolCallRequest::new(
                "call_2".to_string(),
                "Write".to_string(),
                json!({"file_path": "test.rs", "content": "new content"}),
            )],
        ),
        BaseMessage::tool_error("call_2", "Error: permission denied"),
        BaseMessage::ai_with_tool_calls(
            "",
            vec![
                crate::messages::ToolCallRequest::new(
                    "call_3a".to_string(),
                    "Grep".to_string(),
                    json!({"pattern": "TODO"}),
                ),
                crate::messages::ToolCallRequest::new(
                    "call_3b".to_string(),
                    "Bash".to_string(),
                    json!({"command": "ls"}),
                ),
            ],
        ),
        BaseMessage::tool_error("call_3a", "Error: Search timed out"),
        BaseMessage::tool_result("call_3b", "file1.rs\nfile2.rs"),
    ];

    let (msgs, _) = ChatAnthropic::messages_to_anthropic(&messages);

    // 收集所有 tool_result block
    let mut tool_results: Vec<serde_json::Value> = Vec::new();
    for msg in &msgs {
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block["type"] == "tool_result" {
                    tool_results.push(block.clone());
                }
            }
        }
    }

    // 应该有 4 个 tool_result（call_1 success, call_2 error, call_3a error, call_3b success）
    assert_eq!(
        tool_results.len(),
        4,
        "应有 4 个 tool_result block，实际: {}",
        tool_results.len()
    );

    for (i, tr) in tool_results.iter().enumerate() {
        let has_id = tr
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        assert!(has_id, "第 {} 个 tool_result 缺少非空 id 字段: {:?}", i, tr);
    }

    // 验证 tool_result 顺序与 tool_use 顺序一致
    // call_1 → call_2 → call_3a → call_3b
    assert_eq!(tool_results[0]["tool_use_id"], "call_1");
    assert_eq!(tool_results[1]["tool_use_id"], "call_2");
    assert_eq!(
        tool_results[2]["tool_use_id"], "call_3a",
        "call_3a 应在 call_3b 之前"
    );
    assert_eq!(tool_results[3]["tool_use_id"], "call_3b");
}

/// BaseModel::build_request_body 返回 Anthropic Provider-native 请求体
/// （含 `{name, description, input_schema}` 工具格式和顶层 `system` 字段）。
/// 关键不变量：与 Anthropic 实际 invoke 请求体完全一致。
#[test]
fn test_base_model_build_request_body_returns_provider_native_anthropic_format() {
    use crate::llm::types::LlmRequest;
    use crate::llm::BaseModel;
    use crate::tools::ToolDefinition;

    let llm = ChatAnthropic::new("sk-test", "claude-sonnet-4");
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
        .expect("ChatAnthropic 应 override build_request_body 返回 Some");
    // 工具格式：Anthropic-native（无 type:function 嵌套，input_schema 而非 parameters）
    let tools = body["tools"].as_array().expect("tools 应是 array");
    assert_eq!(tools[0]["name"], "Read");
    assert_eq!(tools[0]["description"], "read a file");
    assert!(
        tools[0].get("input_schema").is_some(),
        "Anthropic 工具必须用 input_schema（而非 parameters）"
    );
    assert!(
        tools[0].get("parameters").is_none(),
        "Anthropic 工具不应有 parameters 字段（OpenAI 抽象格式）"
    );
    assert!(
        tools[0].get("type").is_none(),
        "Anthropic 工具不应有 type:function 嵌套（OpenAI 格式）"
    );
    // system 应在顶层（Anthropic 风格），而非 messages 内
    let system = body.get("system").expect("Anthropic 应有顶层 system 字段");
    let system_str = system.as_str().unwrap_or_else(|| {
        // 启用 cache 时可能是 array，取第一个块的 text
        system
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|b| b.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    });
    assert!(
        system_str.contains("SYSTEM_PROMPT"),
        "顶层 system 应包含 base system，实际: {}",
        system_str
    );
    let messages = body["messages"].as_array().expect("messages 应是 array");
    // Anthropic 把 system 从 messages 移除，user 消息应在 messages[0]
    assert_eq!(messages[0]["role"], "user");
}

/// 验证连续多条 Tool 消息合并后 tool_result 顺序与 tool_use 一致
#[test]
fn test_consecutive_tool_results_preserve_order() {
    use crate::messages::ToolCallRequest;

    let messages = vec![
        BaseMessage::human("run three commands"),
        BaseMessage::ai_with_tool_calls(
            "",
            vec![
                ToolCallRequest::new(
                    "call_a".to_string(),
                    "Bash".to_string(),
                    json!({"command": "echo a"}),
                ),
                ToolCallRequest::new(
                    "call_b".to_string(),
                    "Bash".to_string(),
                    json!({"command": "echo b"}),
                ),
                ToolCallRequest::new(
                    "call_c".to_string(),
                    "Bash".to_string(),
                    json!({"command": "echo c"}),
                ),
            ],
        ),
        BaseMessage::tool_result("call_a", "a"),
        BaseMessage::tool_result("call_b", "b"),
        BaseMessage::tool_result("call_c", "c"),
    ];

    let (msgs, _) = ChatAnthropic::messages_to_anthropic(&messages);

    // 找到 user 消息中的 tool_result blocks
    let user_msg = msgs.iter().find(|m| {
        m["role"] == "user"
            && m["content"]
                .as_array()
                .map_or(false, |arr| arr.iter().any(|b| b["type"] == "tool_result"))
    });
    let content = user_msg.unwrap()["content"].as_array().unwrap();

    let tool_results: Vec<_> = content
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .collect();

    assert_eq!(tool_results.len(), 3, "应有 3 个 tool_result");
    assert_eq!(
        tool_results[0]["tool_use_id"], "call_a",
        "第一个应为 call_a"
    );
    assert_eq!(
        tool_results[1]["tool_use_id"], "call_b",
        "第二个应为 call_b"
    );
    assert_eq!(
        tool_results[2]["tool_use_id"], "call_c",
        "第三个应为 call_c"
    );
}
