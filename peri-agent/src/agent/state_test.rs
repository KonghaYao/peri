use super::*;

#[test]
fn test_agent_state_new() {
    let state = AgentState::new("/workspace");
    assert_eq!(state.cwd(), "/workspace");
    assert_eq!(state.messages().len(), 0);
    assert_eq!(state.current_step(), 0);
}

#[test]
fn test_agent_state_messages() {
    let mut state = AgentState::new("/workspace");
    state.add_message(BaseMessage::human("hello"));
    state.add_message(BaseMessage::ai("hi there"));
    assert_eq!(state.messages().len(), 2);
    assert!(matches!(state.messages()[0], BaseMessage::Human { .. }));
}

#[test]
fn test_agent_state_context() {
    let state = AgentState::new("/workspace")
        .with_context("key1", "value1")
        .with_context("key2", "value2");
    assert_eq!(state.get_context("key1"), Some("value1"));
    assert_eq!(state.get_context("missing"), None);
}

#[test]
fn test_token_tracker_default() {
    let state = AgentState::new("/tmp");
    assert_eq!(state.token_tracker().llm_call_count, 0);
    assert_eq!(state.token_tracker().total_input_tokens, 0);
}

#[test]
fn test_token_tracker_accumulate() {
    use peri_model::TokenUsage;
    let mut state = AgentState::new("/tmp");
    state.token_tracker_mut().accumulate(&TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: Some(30),
        cache_read_input_tokens: None,
    });
    assert_eq!(state.token_tracker().total_input_tokens, 100);
    assert_eq!(state.token_tracker().llm_call_count, 1);
}

#[test]
fn test_recall_push_and_drain() {
    let mut state = AgentState::new("/workspace");
    // 初始状态 recall buffer 为空
    assert!(
        state.drain_recall().is_empty(),
        "新创建的 state recall 应为空"
    );
    // push 两条记录
    state.push_recall("MCP Sentry connected".to_string());
    state.push_recall("Cron task registered".to_string());
    assert_eq!(state.recall_buffer.len(), 2, "push 后应有 2 条记录");
    // drain 返回所有记录并清空
    let items = state.drain_recall();
    assert_eq!(
        items,
        vec!["MCP Sentry connected", "Cron task registered"],
        "drain 应按顺序返回所有记录"
    );
    // 第二次 drain 为空（drain 是破坏性操作）
    assert!(state.drain_recall().is_empty(), "drain 后再次 drain 应为空");
}

#[test]
fn test_recall_not_persisted() {
    let mut state = AgentState::new("/workspace");
    state.push_recall("some event".to_string());
    // 序列化后不应包含 recall_buffer 字段
    let json = serde_json::to_string(&state).unwrap();
    assert!(
        !json.contains("recall_buffer"),
        "recall_buffer 不应出现在序列化结果中"
    );
    // 反序列化后 recall buffer 为空
    let mut restored: AgentState = serde_json::from_str(&json).unwrap();
    assert!(
        restored.drain_recall().is_empty(),
        "反序列化后 recall 应为空"
    );
}

#[test]
fn test_recall_injects_as_multiblock() {
    use crate::messages::{ContentBlock, MessageContent};

    let mut state = AgentState::new("/workspace");
    state.push_recall("[MCP] Sentry connected".into());
    state.push_recall("[MCP] Slack connected".into());

    let recalls = state.drain_recall();
    let user_text = "帮我修一下 bug".to_string();

    let content = if recalls.is_empty() {
        MessageContent::text(user_text)
    } else {
        let reminder = format!(
            "<system-reminder>\n{}\n</system-reminder>",
            recalls.join("\n")
        );
        MessageContent::blocks(vec![
            ContentBlock::text(user_text),
            ContentBlock::text(reminder),
        ])
    };

    let msg = BaseMessage::human(content);
    let blocks = msg.content_blocks();
    assert_eq!(blocks.len(), 2);

    let texts: Vec<&str> = blocks.iter().filter_map(|b| b.as_text()).collect();
    assert_eq!(texts[0], "帮我修一下 bug");
    assert!(texts[1].contains("<system-reminder>"));
    assert!(texts[1].contains("Sentry connected"));
    assert!(texts[1].contains("Slack connected"));

    // drain 是破坏性操作，buffer 已清空
    assert!(state.drain_recall().is_empty());
}
