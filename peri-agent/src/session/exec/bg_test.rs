//! BgCommand 单元测试（L5：自 peri-acp/src/host/exec/bg_test.rs 随迁；
//! Phase 5 Step 2：TextChunk 伪消息断言 → CommandFeedback 字段断言）。
//! 注册表装配面测试（`CommandRegistry::register_builtins` 含 `core:bg`、
//! `resolve` 解析）留 ACP——依赖 ACP 命令注册表。

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::command::{
    BgForkRequest, BgForkSpawner, CommandContext, CommandHandler, CommandOutcome, DependencyBag,
    FeedbackChannel, FeedbackLevel, PromptStopReason,
};
use peri_acp_types::event::{EventSink, ExecutorEvent};
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::store::ThreadStore;
use peri_acp_types::thread::{ThreadId, ThreadMeta};

use super::BgCommand;

// ── Mock EventSink ────────────────────────────────────────────────────────

struct MockEventSink {
    events: Mutex<Vec<(String, String)>>,
    push_done_count: Mutex<usize>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            push_done_count: Mutex::new(0),
        }
    }

    fn events(&self) -> Vec<(String, String)> {
        self.events.lock().unwrap().clone()
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

#[async_trait]
impl EventSink for MockEventSink {
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.events
            .lock()
            .unwrap()
            .push((session_id.to_string(), json));
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
    }
}

// ── Mock BgForkSpawner ─────────────────────────────────────────────────────

/// 可注入 mock：`fail` 为 true 时 `spawn_fork` 返回用户可见错误。
struct MockBgForkSpawner {
    fail: bool,
}

impl MockBgForkSpawner {
    fn ok() -> Self {
        Self { fail: false }
    }

    fn failing() -> Self {
        Self { fail: true }
    }
}

#[async_trait]
impl BgForkSpawner for MockBgForkSpawner {
    async fn spawn_fork(&self, _req: BgForkRequest) -> Result<(), String> {
        if self.fail {
            Err("mock spawn 失败".to_string())
        } else {
            Ok(())
        }
    }
}

// ── Mock ThreadStore（BgForkRequest 必需项，极简 no-op 实现）───────────────

struct NoopThreadStore;

#[async_trait]
impl ThreadStore for NoopThreadStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        Ok(meta.id)
    }

    async fn append_messages(&self, _id: &ThreadId, _msgs: &[BaseMessage]) -> Result<()> {
        Ok(())
    }

    async fn load_messages(&self, _id: &ThreadId) -> Result<Vec<BaseMessage>> {
        Ok(Vec::new())
    }

    async fn load_meta(&self, _id: &ThreadId) -> Result<ThreadMeta> {
        Ok(ThreadMeta::new("/tmp"))
    }

    async fn update_meta(&self, _id: &ThreadId, _meta: ThreadMeta) -> Result<()> {
        Ok(())
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        Ok(Vec::new())
    }

    async fn delete_thread(&self, _id: &ThreadId) -> Result<()> {
        Ok(())
    }

    async fn load_context(&self, _thread_id: &ThreadId) -> Result<Vec<BaseMessage>> {
        Ok(Vec::new())
    }

    async fn list_child_threads(&self, _parent_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        Ok(Vec::new())
    }

    async fn list_session_threads(&self, _root_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        Ok(Vec::new())
    }

    async fn update_thread_status(&self, _id: &ThreadId, _status: &str) -> Result<()> {
        Ok(())
    }

    async fn invalidate_context_cache(&self, _thread_id: &ThreadId) -> Result<()> {
        Ok(())
    }

    async fn delete_messages(
        &self,
        _thread_id: &ThreadId,
        _message_ids: &[MessageId],
    ) -> Result<()> {
        Ok(())
    }
}

// ── ctx 构造辅助 ───────────────────────────────────────────────────────────

fn make_ctx(sink: Arc<dyn EventSink>, args: &str) -> CommandContext {
    // Phase 2 拆层：deps 私有化后构造面封闭，core 5 字段经 new() 就位；
    // 非默认旧字段显式赋值（args）。
    let mut ctx = CommandContext::new(
        "test-session".to_string(),
        vec![],
        "/tmp".to_string(),
        sink,
        tokio_util::sync::CancellationToken::new(),
        DependencyBag::new(),
    );
    ctx.args = args.to_string();
    ctx
}

/// 注入 spawner 的完整装配 ctx（executor 内部路径等价形态）：
/// spawner 经 deps 按 `Arc<dyn BgForkSpawner>` 接口注入（注入契约见
/// `CommandContext::dep` doc），bg_event_sender / thread_store 为旧字段。
fn make_ctx_with_spawner(
    sink: Arc<dyn EventSink>,
    args: &str,
    spawner: Arc<dyn BgForkSpawner>,
) -> CommandContext {
    let mut deps = DependencyBag::new();
    deps.insert(
        std::any::TypeId::of::<Arc<dyn BgForkSpawner>>(),
        Arc::new(spawner) as Arc<dyn std::any::Any + Send + Sync>,
    );
    let mut ctx = CommandContext::new(
        "test-session".to_string(),
        vec![],
        "/tmp".to_string(),
        sink,
        tokio_util::sync::CancellationToken::new(),
        deps,
    );
    ctx.args = args.to_string();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    ctx.bg_event_sender = Some(tx);
    ctx.thread_store = Some(Arc::new(NoopThreadStore));
    ctx
}

// ── BgCommand 属性测试 ────────────────────────────────────────────────────

/// 执行并解包：bg 恒 Done，其他变体 panic（与命令层转发 unreachable! 同语义）。
async fn execute_bg(
    cmd: &BgCommand,
    ctx: CommandContext,
) -> peri_acp_types::command::CommandResult {
    match CommandHandler::execute(cmd, ctx).await {
        CommandOutcome::Done(r) => r,
        _ => panic!("bg 应恒 Done"),
    }
}

#[test]
fn test_bg_command_name_and_aliases() {
    // Phase 5 Step 6：旧 AgentCommand trait 已删，元数据取命令关联常量
    //（注册条目挂载的单一事实源）。
    assert_eq!(BgCommand::NAME, "bg");
    assert!(
        BgCommand::ALIASES.contains(&"background"),
        "应包含 background 别名"
    );
    assert!(!BgCommand::DESCRIPTION.is_empty());
}

// ── 空参数测试 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_bg_command_empty_prompt_shows_usage() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), "");
    let cmd = BgCommand;

    let result = execute_bg(&cmd, ctx).await;

    // 应返回空消息 + EndTurn
    assert_eq!(result.messages.len(), 0);
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 用法提示收敛为 CommandFeedback（UiOnly），不再推送 TextChunk 伪消息
    let fb = result.feedback.expect("空参数应返回用法反馈");
    assert_eq!(fb.level, FeedbackLevel::Info);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert!(
        fb.message.contains("用法"),
        "反馈应含用法提示，实际: {}",
        fb.message
    );
    assert!(
        fb.message.contains("/bg"),
        "反馈应含命令名 /bg，实际: {}",
        fb.message
    );

    // 不再产生任何事件（TextChunk 伪消息通道已退役）
    assert!(
        sink.events().is_empty(),
        "空参数不应推送事件（反馈走 CommandFeedback），实际: {:?}",
        sink.events()
    );
}

#[tokio::test]
async fn test_bg_command_does_not_call_push_done_itself() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), "");
    let cmd = BgCommand;

    let _result = execute_bg(&cmd, ctx).await;

    // BgCommand 自身不应调用 push_done（由 executor 负责）
    let count = sink.push_done_count();
    assert_eq!(
        count, 0,
        "BgCommand 自身不应调用 push_done，由 executor 负责"
    );
}

// ── 缺省 bg 上下文优雅降级测试（S1.2）───────────────────────────────────────

/// [S1.2] 公开 RPC（session/execute-command / session/rewind）传 None 时
/// /bg 不得 panic——spawner 缺失改为 feedback(Error) + EndTurn 返回。
#[tokio::test]
async fn test_bg_command_missing_bg_context_gracefully_fails() {
    let sink = Arc::new(MockEventSink::new());
    // deps 空表（RPC 直调缺装配面，spawner 未注入）
    let ctx = make_ctx(sink.clone(), "整理周报");
    let cmd = BgCommand;

    let result = execute_bg(&cmd, ctx).await;

    // 不 panic，正常返回 EndTurn
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(result.messages.len(), 0);

    // 错误收敛为 CommandFeedback（Error / UiOnly），指明缺失的装配面
    let fb = result.feedback.expect("spawner 缺失应返回错误反馈");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert!(
        fb.message.contains("未配置"),
        "错误反馈应指明缺失字段，实际: {}",
        fb.message
    );

    // 不再产生任何事件（错误提示不再伪装 TextChunk）
    assert!(
        sink.events().is_empty(),
        "缺失装配面不应推送事件，实际: {:?}",
        sink.events()
    );
}

// ── 成功路径 / spawn 失败测试（Phase 5 Step 2 新增）────────────────────────

/// 成功路径：spawner 注入 + spawn_fork Ok → Info 反馈（UiOnly），
/// prompt 前 80 字符截断（CJK-safe: chars().take(80)）。
#[tokio::test]
async fn test_bg_command_success_returns_confirmation_feedback() {
    let sink = Arc::new(MockEventSink::new());
    let spawner: Arc<dyn BgForkSpawner> = Arc::new(MockBgForkSpawner::ok());
    let ctx = make_ctx_with_spawner(sink.clone(), "整理周报", spawner);
    let cmd = BgCommand;

    let result = execute_bg(&cmd, ctx).await;

    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(result.messages.len(), 0, "后台任务不进主会话历史");

    let fb = result.feedback.expect("成功应返回启动确认反馈");
    assert_eq!(fb.level, FeedbackLevel::Info);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert!(
        fb.message.starts_with("◆ 后台任务已启动: "),
        "确认反馈应有前缀，实际: {}",
        fb.message
    );
    assert!(fb.message.ends_with("整理周报"));
}

/// 长 prompt 截断：确认反馈只含前 80 字符（CJK-safe truncation）。
#[tokio::test]
async fn test_bg_command_confirmation_truncates_long_prompt() {
    let sink = Arc::new(MockEventSink::new());
    let spawner: Arc<dyn BgForkSpawner> = Arc::new(MockBgForkSpawner::ok());
    let long_prompt = "长".repeat(120);
    let ctx = make_ctx_with_spawner(sink.clone(), &long_prompt, spawner);
    let cmd = BgCommand;

    let result = execute_bg(&cmd, ctx).await;

    let fb = result.feedback.expect("成功应返回启动确认反馈");
    let body = fb.message.strip_prefix("◆ 后台任务已启动: ").unwrap();
    assert_eq!(body.chars().count(), 80, "确认反馈应截断为前 80 字符");
    assert!(body.ends_with("长"));
}

/// spawn_fork Err → feedback(Error, 用户可见错误, UiOnly)。
#[tokio::test]
async fn test_bg_command_spawn_fork_error_returns_error_feedback() {
    let sink = Arc::new(MockEventSink::new());
    let spawner: Arc<dyn BgForkSpawner> = Arc::new(MockBgForkSpawner::failing());
    let ctx = make_ctx_with_spawner(sink.clone(), "整理周报", spawner);
    let cmd = BgCommand;

    let result = execute_bg(&cmd, ctx).await;

    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    let fb = result.feedback.expect("spawn 失败应返回错误反馈");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert!(
        fb.message.contains("mock spawn 失败"),
        "错误反馈应透传 spawner 错误，实际: {}",
        fb.message
    );
}
