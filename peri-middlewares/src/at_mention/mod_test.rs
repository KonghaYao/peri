//! @path reads through the session Workspace reader, never through the host cwd.
use std::{collections::HashMap, path::Path, sync::Arc};

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
use crate::workspace_io::{WorkspaceMentionContent, WorkspaceReadError};

#[path = "work_fixture.rs"]
mod work_fixture;

struct Reader(HashMap<String, String>);

#[async_trait]
impl WorkspaceFileReader for Reader {
    async fn read_text(&self, _path: &Path) -> Result<String, WorkspaceReadError> {
        Err(WorkspaceReadError::Unavailable)
    }

    async fn read_mention(
        &self,
        path: &str,
        line_start: Option<usize>,
        line_end: Option<usize>,
    ) -> Result<WorkspaceMentionContent, WorkspaceReadError> {
        let content = self.0.get(path).ok_or(WorkspaceReadError::ReadFailed)?;
        Ok(WorkspaceMentionContent {
            path: path.to_string(),
            content: content.clone(),
            line_start,
            line_end,
            truncated: false,
            is_dir: false,
        })
    }
}

fn reader(entries: &[(&str, &str)]) -> Arc<dyn WorkspaceFileReader> {
    Arc::new(Reader(
        entries
            .iter()
            .map(|(path, content)| (path.to_string(), content.to_string()))
            .collect(),
    ))
}

#[tokio::test]
async fn no_mentions_no_injection() {
    let mw = AtMentionMiddleware::new(reader(&[]));
    let mut state = AgentState::default();
    state.add_message(BaseMessage::human("你好世界"));
    mw.before_agent(&mut state).await.unwrap();
    assert_eq!(state.messages().len(), 1);
}

#[tokio::test]
async fn mention_uses_workspace_content_even_with_different_host_file() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.rs"), "host secret").unwrap();
    let mw = AtMentionMiddleware::new(reader(&[("test.rs", "remote content")]));
    let mut state = AgentState::default();
    state.cwd = dir.path().to_string_lossy().into_owned();
    state.add_message(BaseMessage::human("看看 @test.rs"));

    mw.before_agent(&mut state).await.unwrap();
    assert_eq!(state.messages().len(), 3);
    assert!(state.messages()[1].has_tool_calls());
    let tool_use = serde_json::to_value(&state.messages()[1]).unwrap();
    assert_eq!(tool_use["content"][0]["name"], "workspace/readMention");
    let output = state.messages()[2].content();
    assert!(output.contains("remote content"));
    assert!(!output.contains("host secret"));
}

#[tokio::test]
async fn unavailable_workspace_never_reads_host_file() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.rs"), "host secret").unwrap();
    let mw = AtMentionMiddleware::new(reader(&[]));
    let mut state = AgentState::default();
    state.cwd = dir.path().to_string_lossy().into_owned();
    state.add_message(BaseMessage::human("看看 @test.rs"));

    mw.before_agent(&mut state).await.unwrap();
    assert_eq!(state.messages().len(), 1);
}

#[tokio::test]
async fn range_mention_records_actual_workspace_request_and_line_prefix() {
    let mw = AtMentionMiddleware::new(reader(&[("sample.txt", "second\nthird")]));
    let mut state = AgentState::default();
    state.add_message(BaseMessage::human("看 @sample.txt#L2-3"));
    mw.before_agent(&mut state).await.unwrap();
    let tool_use = serde_json::to_value(&state.messages()[1]).unwrap();
    assert_eq!(tool_use["content"][0]["name"], "workspace/readMention");
    assert_eq!(tool_use["content"][0]["input"]["lineStart"], 2);
    assert_eq!(tool_use["content"][0]["input"]["lineEnd"], 3);
    assert_eq!(
        state.messages()[2].content(),
        "→ sample.txt (L2-L3)\nsecond\nthird"
    );
}

/// Batch input is read once; historic @path text is not replayed.
#[tokio::test]
async fn mention_batch_reads_first_input_without_replaying_history() {
    let dir = tempdir().unwrap();
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
    let mut ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build();
    let _work_fixture = work_fixture::bind(&mut ctx).await;
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(AtMentionMiddleware::new(reader(&[(
        "fresh.txt",
        "本批需要读取的内容",
    )]))));
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
    assert_eq!(messages.len(), 6);
    assert!(messages[4].has_tool_calls());
    assert!(messages[5].content().contains("本批需要读取的内容"));
    for original in [&old, &first, &last] {
        assert_eq!(
            serde_json::to_value(transcript.get(original.id()).unwrap().message()).unwrap(),
            serde_json::to_value(original).unwrap(),
        );
    }
}

#[tokio::test]
async fn explicit_empty_batch_does_not_read_history() {
    let dir = tempdir().unwrap();
    let original = BaseMessage::human("旧输入 @old.txt");
    let session = Session::new(
        Arc::from(dir.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    session.transcript().write().append(original.clone());
    let mut ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build();
    let _work_fixture = work_fixture::bind(&mut ctx).await;
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(AtMentionMiddleware::new(reader(&[(
        "old.txt", "not read",
    )]))));
    ctx.runtime.middleware_chain = Arc::new(chain);
    run_before_agent(&ctx, &[]).await.unwrap();
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    assert_eq!(
        serde_json::to_value(transcript.get(original.id()).unwrap().message()).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
}
