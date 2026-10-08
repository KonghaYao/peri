//! @path reads through the session Workspace reader, never through the host cwd.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{collections::HashMap, path::Path, sync::Arc};

use peri_agent::agent::state::AgentState;
use peri_agent::{
    agent::stages::{
        middleware_runner::{run_before_agent, run_before_input},
        receive::run_receive,
        ReceiveInput, StageContext,
    },
    middleware::MiddlewareChain,
    session::{FrozenContext, MessageSource, QueuedMessage, Session},
};
use tempfile::tempdir;

use super::*;
use crate::workspace_io::{WorkspaceMentionContent, WorkspaceReadError};

/// [回归测试] 续读位置以模型实际收到的文本为准，无需冗余结果字段。
#[test]
fn trimmed_content_renders_requested_range_resume_line() {
    let rendered = trim_mention_content("甲\n乙\n丙", Some(5), 7);
    assert!(rendered.text.contains("已截断"));
    assert!(rendered.text.starts_with("甲\n"));
    assert!(rendered.text.contains("从 L6 继续读取"));
}

#[path = "work_fixture.rs"]
pub(crate) mod work_fixture;

/// 测试 Workspace reader：按路径给正文，并记录实际读取次数（预算闸门要先于读取）。
struct Reader {
    entries: HashMap<String, String>,
    reads: Arc<AtomicUsize>,
}

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
        self.reads.fetch_add(1, Ordering::SeqCst);
        let content = self
            .entries
            .get(path)
            .ok_or(WorkspaceReadError::ReadFailed)?;
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
    Arc::new(Reader {
        entries: entries
            .iter()
            .map(|(path, content)| (path.to_string(), content.to_string()))
            .collect(),
        reads: Arc::new(AtomicUsize::new(0)),
    })
}

fn counting_reader(
    entries: &[(&str, &str)],
    reads: &Arc<AtomicUsize>,
) -> Arc<dyn WorkspaceFileReader> {
    Arc::new(Reader {
        entries: entries
            .iter()
            .map(|(path, content)| (path.to_string(), content.to_string()))
            .collect(),
        reads: Arc::clone(reads),
    })
}

/// 生产 runner 上下文 + 单一 AtMention 中间件。
async fn context_with(
    cwd: &Path,
    middleware: AtMentionMiddleware,
) -> (StageContext, tempfile::TempDir) {
    let session = Session::new(
        Arc::from(cwd.to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    let mut ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .build();
    let fixture = work_fixture::bind(&mut ctx).await;
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(middleware));
    ctx.runtime.middleware_chain = Arc::new(chain);
    (ctx, fixture)
}

/// 走生产 Receive：把入队消息写入 transcript 并返回本批身份。
async fn receive(ctx: &StageContext) -> Vec<peri_agent::messages::MessageId> {
    run_receive(ReceiveInput {
        context: ctx.clone(),
    })
    .await
    .unwrap()
    .input_message_ids
}

#[tokio::test]
async fn no_mentions_no_injection() {
    let dir = tempdir().unwrap();
    let mw = AtMentionMiddleware::new(reader(&[]));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let input = BaseMessage::human("你好世界");
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        input.clone(),
    ));
    let ids = receive(&ctx).await;
    run_before_agent(&ctx, &ids).await.unwrap();
    assert_eq!(ctx.session.transcript.read().len(), 1);
}

#[tokio::test]
async fn mention_uses_workspace_content_even_with_different_host_file() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.rs"), "host secret").unwrap();
    let mw = AtMentionMiddleware::new(reader(&[("test.rs", "remote content")]));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let input = BaseMessage::human("看看 @test.rs");
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        input.clone(),
    ));
    let ids = receive(&ctx).await;
    run_before_agent(&ctx, &ids).await.unwrap();

    let transcript = ctx.session.transcript.read();
    let messages = transcript.visible_messages();
    assert_eq!(messages.len(), 3);
    assert!(messages[1].has_tool_calls());
    let tool_use = serde_json::to_value(messages[1]).unwrap();
    assert_eq!(tool_use["content"][0]["name"], "workspace/readMention");
    let output = messages[2].content();
    assert!(output.contains("remote content"));
    assert!(!output.contains("host secret"));
}

#[tokio::test]
async fn unavailable_workspace_never_reads_host_file() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.rs"), "host secret").unwrap();
    let mw = AtMentionMiddleware::new(reader(&[]));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let input = BaseMessage::human("看看 @test.rs");
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        input.clone(),
    ));
    let ids = receive(&ctx).await;
    run_before_agent(&ctx, &ids).await.unwrap();

    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.visible_messages().len(), 1);
    assert_eq!(transcript.visible_messages()[0].content(), "看看 @test.rs");
}

/// 无批次身份的 legacy 适配器：不注入、也不扫描历史最后一条 Human。
#[tokio::test]
async fn adapter_without_batch_identity_never_scans_history() {
    let mw = AtMentionMiddleware::new(reader(&[("old.txt", "历史内容不应被注入")]));
    let mut state = AgentState::default();
    state.add_message(BaseMessage::human("旧输入 @old.txt"));
    mw.before_input(&mut state).await.unwrap();
    assert_eq!(state.messages().len(), 1);
    assert_eq!(state.messages()[0].content(), "旧输入 @old.txt");
}

#[tokio::test]
async fn range_mention_records_actual_workspace_request_and_line_prefix() {
    let dir = tempdir().unwrap();
    let mw = AtMentionMiddleware::new(reader(&[("sample.txt", "second\nthird")]));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let input = BaseMessage::human("看 @sample.txt#L2-3");
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        input.clone(),
    ));
    let ids = receive(&ctx).await;
    run_before_agent(&ctx, &ids).await.unwrap();

    let transcript = ctx.session.transcript.read();
    assert_eq!(
        transcript.visible_messages()[2].content(),
        "→ sample.txt (L2-L3)\nsecond\nthird"
    );
    let tool_use = serde_json::to_value(transcript.visible_messages()[1]).unwrap();
    assert_eq!(tool_use["content"][0]["name"], "workspace/readMention");
    assert_eq!(tool_use["content"][0]["input"]["lineStart"], 2);
    assert_eq!(tool_use["content"][0]["input"]["lineEnd"], 3);
}

/// 单项正文超字节预算：截断必须带显式说明，且给出可继续读取的位置。
#[tokio::test]
async fn oversized_single_line_is_byte_budgeted_with_explicit_notice() {
    let dir = tempdir().unwrap();
    let huge_line = "x".repeat(MAX_MENTION_CONTENT_BYTES * 2);
    let mw = AtMentionMiddleware::new(reader(&[("huge.txt", &huge_line)]));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let input = BaseMessage::human("看 @huge.txt");
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        input.clone(),
    ));
    let ids = receive(&ctx).await;
    run_before_agent(&ctx, &ids).await.unwrap();

    let transcript = ctx.session.transcript.read();
    let output = transcript.visible_messages()[2].content();
    assert!(
        output.len() < MAX_MENTION_CONTENT_BYTES + 512,
        "注入正文必须落在字节预算内：{}",
        output.len()
    );
    assert!(
        output.contains("单行超过") && output.contains("UTF-8 边界截断"),
        "单行超预算必须显式说明：{output}"
    );
    assert!(output.contains("请用更小的行范围重新读取"));
}

/// 多行正文超字节预算：截断落在行边界并给出续读行号。
#[tokio::test]
async fn oversized_multiline_content_reports_resume_line() {
    let dir = tempdir().unwrap();
    let line = "y".repeat(1024);
    let content: String = (0..64).map(|_| format!("{line}\n")).collect();
    let mw = AtMentionMiddleware::new(reader(&[("multi.txt", &content)]));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let input = BaseMessage::human("看 @multi.txt");
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        input.clone(),
    ));
    let ids = receive(&ctx).await;
    run_before_agent(&ctx, &ids).await.unwrap();

    let transcript = ctx.session.transcript.read();
    let output = transcript.visible_messages()[2].content();
    assert!(
        output.contains("从 L32 继续读取"),
        "按行截断必须给出可继续读取位置：{output}"
    );
    assert!(output.len() < MAX_MENTION_CONTENT_BYTES + 512);
}

/// 整批累计超预算：后续提及**不读**，改为显式「未载入」回执（不静默裁掉）。
#[tokio::test]
async fn batch_budget_stops_reads_and_reports_not_loaded() {
    let dir = tempdir().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    // 每项 20 KiB：前 6 项吃掉 120 KiB，接近 128 KiB 的整批预算；第 8 项必然
    // 只能用剩余预算，最后一项在预算耗尽后不读。
    let body = "z".repeat(20 * 1024);
    let entries: Vec<(String, String)> = (0..8)
        .map(|index| (format!("f{index}.txt"), body.clone()))
        .collect();
    let refs: Vec<(&str, &str)> = entries
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    let mw = AtMentionMiddleware::new(counting_reader(&refs, &reads));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let mention = (0..8)
        .map(|index| format!("@f{index}.txt"))
        .collect::<Vec<_>>()
        .join(" ");
    let input = BaseMessage::human(format!("看 {mention}"));
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        input.clone(),
    ));
    let ids = receive(&ctx).await;
    run_before_agent(&ctx, &ids).await.unwrap();

    let transcript = ctx.session.transcript.read();
    let messages = transcript.visible_messages();
    // 每个提及都有一对调用 + 结果（成功或显式回执），不留静默缺口。
    let tool_use_count: usize = messages
        .iter()
        .filter(|message| message.has_tool_calls())
        .map(|message| message.tool_calls().len())
        .sum();
    assert_eq!(tool_use_count, 8, "8 个提及各配对一次假调用");
    let result_count = messages
        .iter()
        .filter(|message| matches!(message, BaseMessage::Tool { .. }))
        .count();
    assert_eq!(result_count, 8, "8 个提及各一条结果（成功或未载入回执）");
    let injected_bytes: usize = messages[1..]
        .iter()
        .map(|message| message.content().len())
        .sum();
    assert!(
        injected_bytes < MAX_BATCH_MENTION_BYTES + 4096,
        "整批注入必须落在批预算内：{injected_bytes}"
    );
    assert!(
        reads.load(Ordering::SeqCst) < 8,
        "预算闸门必须先于读取，实际读取量必须受限：{}",
        reads.load(Ordering::SeqCst)
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content().contains("注入预算已用尽，未载入该文件")),
        "预算耗尽的提及必须给出显式未载入回执"
    );
}

/// 同一 loop 的中途批次：只处理本批新输入，不重读历史。
#[tokio::test]
async fn mid_loop_batch_prepares_only_new_inputs() {
    let dir = tempdir().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let mw = AtMentionMiddleware::new(counting_reader(
        &[("first.txt", "first body"), ("steered.txt", "steered body")],
        &reads,
    ));
    let (ctx, _fixture) = context_with(dir.path(), mw).await;
    let first = BaseMessage::human("看 @first.txt");
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        first.clone(),
    ));
    let first_ids = receive(&ctx).await;
    run_before_agent(&ctx, &first_ids).await.unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    // 首批注入恰有一次（此处快照：后续 Receive 会镜像已提交投影，可能重放
    // 同一批消息，不是本中间件的重复注入）。
    assert_eq!(
        ctx.session
            .transcript
            .read()
            .visible_messages()
            .iter()
            .filter(|message| message.content().contains("first body"))
            .count(),
        1,
        "首批内容只注入一次"
    );

    // 中途批次（steering / SDK 追加）：Receive 会把新输入写进 transcript 并把
    // 本批身份交给 `run_before_input`（此处直接构造该批次，同一生产入口）。
    let steering = BaseMessage::human("再补一份 @steered.txt");
    ctx.session.transcript.write().append(steering.clone());
    run_before_input(&ctx, &[steering.id()]).await.unwrap();

    let transcript = ctx.session.transcript.read();
    let messages = transcript.visible_messages();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "首批与中途批次各读一次，历史不重读"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content().contains("first body")),
        "首批内容仍在会话里"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.content().contains("steered body"))
            .count(),
        1,
        "中途批次内容必须注入"
    );
    // 纯工具续跑（空批次）不再注入。
    let before = messages.len();
    drop(transcript);
    run_before_input(&ctx, &[]).await.unwrap();
    assert_eq!(
        ctx.session.transcript.read().visible_messages().len(),
        before
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
