//! Tests for at_mention
use std::{fs, sync::Arc};

use peri_agent::agent::state::AgentState;
use peri_agent::{
    agent::stages::{
        middleware_runner::run_before_agent, receive::run_receive, ReceiveInput, StageContext,
    },
    middleware::MiddlewareChain,
    session::{FrozenContext, MessageSource, QueuedMessage, Session},
};
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

/// [回归测试] 批次前面的 @path 仍应读取，历史引用不应随新批次重复注入。
#[tokio::test]
async fn test_mention_batch_reads_first_input_without_replaying_history() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("old.txt"), "不得重新读取的旧内容").unwrap();
    fs::write(dir.path().join("fresh.txt"), "本批需要读取的内容").unwrap();
    let old = BaseMessage::human("旧输入 @old.txt");
    let first = BaseMessage::human("请查看 @fresh.txt 和 @missing.txt");
    let last = BaseMessage::human("普通文本");
    let session = Session::new(
        Arc::from(dir.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    session.transcript().write().append(old.clone());
    session
        .transcript()
        .write()
        .append(BaseMessage::ai("之前的回复"));
    for input in [&first, &last] {
        session.queue().push(QueuedMessage::prompt(
            MessageSource::UserInput,
            input.clone(),
        ));
    }
    let mut ctx = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(AtMentionMiddleware::new(dir.path().to_path_buf())));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let received = run_receive(ReceiveInput {
        context: ctx.clone(),
    })
    .await
    .unwrap();
    run_before_agent(&ctx, &received.input_message_ids)
        .await
        .unwrap();
    let transcript = ctx.session.transcript.read();
    let messages = transcript.visible_messages();
    assert_eq!(messages.len(), 6, "只为可读的新引用注入一对工具消息");
    assert!(matches!(messages[4], BaseMessage::Ai { .. }));
    assert!(messages[4].has_tool_calls());
    assert!(matches!(messages[5], BaseMessage::Tool { .. }));
    assert!(messages[5].content().contains("本批需要读取的内容"));
    assert!(!messages[5].content().contains("不得重新读取的旧内容"));
    for original in [&old, &first, &last] {
        assert_eq!(
            serde_json::to_value(transcript.get(original.id()).unwrap().message()).unwrap(),
            serde_json::to_value(original).unwrap(),
            "每条用户消息保留原内容和身份"
        );
    }
}

#[tokio::test]
async fn test_mention_explicit_empty_batch_does_not_read_history() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("old.txt"), "不应注入的旧内容").unwrap();
    let original = BaseMessage::human("旧输入 @old.txt");
    let session = Session::new(
        Arc::from(dir.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    session.transcript().write().append(original.clone());
    let mut ctx = StageContext::new(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(AtMentionMiddleware::new(dir.path().to_path_buf())));
    ctx.runtime.middleware_chain = Arc::new(chain);
    run_before_agent(&ctx, &[]).await.unwrap();
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1, "空批次不回退读取旧消息中的引用");
    assert_eq!(
        serde_json::to_value(transcript.get(original.id()).unwrap().message()).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
}
