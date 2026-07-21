//! Tests for at_mention
use std::fs;

use peri_agent::agent::state::AgentState;
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn test_no_mentions_no_injection() {
    // 无 @ 提及时不注入任何消息
    let dir = tempdir().unwrap();
    let mw = AtMentionMiddleware::new(dir.path().to_path_buf());
    let mut state = AgentState::default();
    state.cwd = dir.path().to_string_lossy().to_string();
    state.add_message(BaseMessage::human("你好世界"));

    let before_len = state.messages().len();
    mw.before_agent(&mut state).await.unwrap();
    // 没有注入，消息数不变
    assert_eq!(state.messages().len(), before_len);
}

#[tokio::test]
async fn test_mention_injects_read_tool() {
    // @test.rs 注入 Ai[ToolUse] + Tool[ToolResult] 共 2 条消息
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("test.rs"), "fn main() {}\n").unwrap();
    let mw = AtMentionMiddleware::new(dir.path().to_path_buf());
    let mut state = AgentState::default();
    state.cwd = dir.path().to_string_lossy().to_string();
    state.add_message(BaseMessage::human("看看 @test.rs"));

    mw.before_agent(&mut state).await.unwrap();

    // 1 Human + 1 Ai + 1 Tool = 3
    assert_eq!(state.messages().len(), 3);

    // 第二条是 Ai，包含 ToolUse
    let ai_msg = &state.messages()[1];
    assert!(matches!(ai_msg, BaseMessage::Ai { .. }));
    assert!(ai_msg.has_tool_calls());

    // 第三条是 Tool 结果
    let tool_msg = &state.messages()[2];
    assert!(matches!(tool_msg, BaseMessage::Tool { .. }));
    let tool_content = tool_msg.content();
    assert!(tool_content.starts_with("→ test.rs"));
    assert!(tool_content.contains("fn main() {}"));
}
