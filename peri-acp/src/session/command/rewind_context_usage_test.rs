//! rewind 后的 context 占用快照（设计 §2.2）。
//!
//! 截断使 rewind 前保存的 `threads.context_usage_tokens` 失效：durable 提交成功后
//! 必须写回 NULL（未知），且写失败不得把已提交的 rewind 伪装成失败；没有持久化后端的
//! 打印/测试路径保持原样。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{
    event::ExecutorEvent,
    messages::BaseMessage,
    session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources},
    thread::{CancelPolicy, ThreadId},
    workspace::SessionBinding,
};

use super::super::{
    CommandContext, CommandHandler, CommandOutcome, CommandResult, FeedbackChannel, FeedbackLevel,
};
use super::RewindCommand;

// ── Mock EventSink ────────────────────────────────────────────────────────

struct MockEventSink;

#[async_trait]
impl crate::session::event_sink::EventSink for MockEventSink {
    async fn push_event(
        &self,
        _session_id: &str,
        _event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), peri_acp_types::event::EventSinkError> {
        Ok(())
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

// ── 已绑定会话夹具 ────────────────────────────────────────────────────────

/// 临时库上的已绑定会话（与 compact 持久化测试同形：断言走真实存储）。
struct BoundSession {
    resources: Arc<dyn SessionResources>,
    thread_id: ThreadId,
    cwd: String,
    _db: tempfile::TempDir,
}

impl BoundSession {
    async fn open(file: &str) -> Self {
        let db = tempfile::tempdir().expect("创建临时目录失败");
        let db_path = db.path().join(file);
        let resources: Arc<dyn SessionResources> = Arc::new(
            peri_resources::sessions::SessionResourcesImpl::open(&db_path)
                .await
                .expect("打开会话库失败"),
        );
        let workspace = resources
            .resolve_workspace(db.path())
            .await
            .expect("解析工作区失败");
        let thread_id: ThreadId = uuid::Uuid::now_v7().to_string();
        resources
            .create_session(&NewSession {
                thread_id: thread_id.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
            })
            .await
            .expect("创建会话失败");
        Self {
            resources,
            thread_id,
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            _db: db,
        }
    }

    fn resources(&self) -> Arc<dyn SessionResources> {
        Arc::clone(&self.resources)
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────────

/// 构造 `target` 为第二条消息的历史；目标及其之后全部被 rewind 移除。
fn make_history() -> Vec<BaseMessage> {
    vec![
        BaseMessage::human("保留的问题"),
        BaseMessage::human("回滚目标"),
        BaseMessage::ai("将被移除的回复"),
    ]
}

fn make_ctx(
    resources: Option<Arc<dyn SessionResources>>,
    thread_id: Option<String>,
    history: Vec<BaseMessage>,
    cwd: String,
    target: &str,
) -> CommandContext {
    let mut ctx = CommandContext::new(
        "test-session".to_string(),
        history,
        cwd,
        Arc::new(MockEventSink),
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    );
    ctx.session_resources = resources;
    ctx.thread_id = thread_id;
    ctx.parsed_args = RewindCommand::args_schema().parse(target).ok();
    ctx
}

async fn execute_rewind_cmd(ctx: CommandContext) -> CommandResult {
    match CommandHandler::execute(&RewindCommand, ctx).await {
        CommandOutcome::Done(result) => result,
        _ => panic!("rewind 应恒 Done"),
    }
}

#[tokio::test]
async fn rewind_commit_clears_context_usage_snapshot() {
    let session = BoundSession::open("rewind-context-usage.db").await;
    let history = make_history();
    let payloads: Vec<peri_acp_types::store::PersistedPayload> = history
        .iter()
        .cloned()
        .map(peri_acp_types::store::PersistedPayload::Message)
        .collect();
    session
        .resources
        .append_history(&session.thread_id, &payloads)
        .await
        .expect("持久化历史失败");
    session
        .resources
        .set_context_usage(&session.thread_id, Some(4096))
        .await
        .expect("写入 rewind 前的占用快照失败");
    let target = history[1].id().as_uuid().to_string();

    let ctx = make_ctx(
        Some(session.resources()),
        Some(session.thread_id.clone()),
        history,
        session.cwd.clone(),
        &target,
    );
    let result = execute_rewind_cmd(ctx).await;

    assert_eq!(
        result.feedback.as_ref().map(|feedback| feedback.level),
        Some(FeedbackLevel::Info),
        "durable 提交成功应返回成功反馈"
    );
    assert_eq!(result.messages.len(), 1, "只保留目标之前的消息");
    let meta = session
        .resources
        .load_session_meta(&session.thread_id)
        .await
        .expect("读回会话 metadata 失败");
    assert_eq!(
        meta.context_usage_tokens, None,
        "rewind 后旧占用快照必须失效（NULL = 未知）"
    );
}

#[tokio::test]
async fn rewind_without_persistence_backend_still_succeeds() {
    let history = make_history();
    let target = history[1].id().as_uuid().to_string();

    let ctx = make_ctx(None, None, history, "/tmp".to_string(), &target);
    let result = execute_rewind_cmd(ctx).await;

    assert_eq!(
        result.feedback.as_ref().map(|feedback| feedback.level),
        Some(FeedbackLevel::Info),
        "无持久化后端路径仍应成功"
    );
    assert_eq!(
        result.feedback.as_ref().map(|feedback| feedback.channel),
        Some(FeedbackChannel::UiOnly)
    );
    assert_eq!(result.messages.len(), 1);
}
