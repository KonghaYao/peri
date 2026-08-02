//! executor.rs 单元测试。
//!
//! 重点覆盖 [`intercept_immediate_command`]——命令拦截是 execute_prompt 的
//! 前置短路逻辑，任何回归（如忘记 `push_done`）都会导致 TUI 永久 loading
//! （issue_2026-05-29-immediate-command-missing-push-done）。
//!
//! Mock 命名遵循 CLAUDE.md：`make_` 前缀（函数），`Mock` 前缀（结构体）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_agent::{
    agent::{events::ExecutorEvent, AgentCancellationToken},
    interaction::{InteractionContext, InteractionResponse, UserInteractionBroker},
    messages::{BaseMessage, ContentBlock, ImageSource, MessageContent},
};

use super::{
    intercept_immediate_command, is_keepgoing, run_session_loop, InterceptRequest,
    PromptStopReason, SessionContext, TurnInput,
};
use crate::{
    provider::{LlmProvider, PeriConfig},
    session::{agent_pool::AgentPool, event_sink::EventSink},
};
use peri_middlewares::{
    prelude::{PermissionMode, SharedPermissionMode},
    tool_search::ToolSearchIndex,
};

// ── Mock EventSink ─────────────────────────────────────────────────────────

/// Mock EventSink，记录所有 push_done 调用。
struct MockEventSink {
    push_done_count: Mutex<usize>,
    pushed_events: Mutex<Vec<String>>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            push_done_count: Mutex::new(0),
            pushed_events: Mutex::new(Vec::new()),
        }
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

#[async_trait]
impl EventSink for MockEventSink {
    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.pushed_events.lock().unwrap().push(json);
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str) {
        *self.push_done_count.lock().unwrap() += 1;
    }
}

/// 空操作 broker：短路路径不会触发任何交互，仅满足 SessionContext 构造。
struct NoopBroker;

#[async_trait]
impl UserInteractionBroker for NoopBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        InteractionResponse::Rejected
    }
}

// ── Helper 工厂函数 ─────────────────────────────────────────────────────────

/// 构造最小 InterceptRequest（auxiliary_model / thread_store 等均为 None）。
///
/// 8 个参数全部是测试所需的引用——测试构造函数不强制参数对象化。
#[allow(clippy::too_many_arguments)]
fn make_intercept_request<'a>(
    content: &'a MessageContent,
    history: &'a [BaseMessage],
    session_id: &'a str,
    cancel: &'a AgentCancellationToken,
    peri_config: &'a Arc<PeriConfig>,
    event_sink: &'a Arc<dyn EventSink>,
    bg_event_tx: &'a tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    bg_registry: &'a Arc<peri_middlewares::subagent::BackgroundTaskRegistry>,
) -> InterceptRequest<'a> {
    InterceptRequest {
        content,
        history,
        cwd: "/tmp",
        session_id,
        cancel,
        peri_config,
        event_sink,
        auxiliary_model: &None,
        thread_store: None,
        thread_id: None,
        bg_event_tx,
        bg_registry,
        frozen: None,
    }
}

/// 构造共享的 bg registry + bg channel（拦截测试不实际触发 bg，但需要传入句柄）。
fn make_bg_infra() -> (
    tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    Arc<peri_middlewares::subagent::BackgroundTaskRegistry>,
) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_middlewares::subagent::BackgroundTaskRegistry::new());
    (tx, registry)
}

/// 构造最小 SessionContext（keepgoing 短路路径只用到 session_id，其余字段给默认值）。
fn make_session_context(session_id: &str) -> SessionContext {
    SessionContext {
        provider: LlmProvider::OpenAi {
            api_key: "test-key".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            model: "gpt-4o".to_string(),
            effort: None,
            max_tokens: 32000,
            context_1m: false,
            retry_observer: None,
        },
        peri_config: Arc::new(Default::default()),
        cwd: "/tmp".to_string(),
        session_id: session_id.to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(NoopBroker),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        session_manager: None,
        pool: Arc::new(parking_lot::Mutex::new(AgentPool::new())),
        thread_store: None,
        thread_id: None,
        plugin_skill_roots: vec![],
        plugin_agent_dirs: vec![],
        plugin_loaded: vec![],
        hook_groups: vec![],
        cron_scheduler: None,
        mcp_pool: None,
        channel_state: None,
        tool_search_index: Arc::new(ToolSearchIndex::default()),
        shared_tools: Arc::new(parking_lot::RwLock::new(Default::default())),
        lsp_servers: vec![],
        workflow_executor: None,
        workflow_middleware: None,
        session_start_source: None,
        allow_await_wake: false,
        v2_event_tx: None,
    }
}

// ── intercept_immediate_command: 路径分支测试 ─────────────────────────────

/// 普通 slash 命令（非 Immediate 注册）：不在默认注册表中 → 返回 None
#[tokio::test]
async fn test_intercept_unknown_command_returns_none() {
    // Arrange
    let content = MessageContent::text("/nonexistent");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：未知命令不拦截，继续走 agent 管线
    assert!(result.is_none(), "未知命令应返回 None 继续走 agent 管线");
}

/// 普通文本（无 `/` 前缀）：返回 None
#[tokio::test]
async fn test_intercept_plain_text_returns_none() {
    // Arrange
    let content = MessageContent::text("你好，请帮我写代码");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：普通文本不拦截
    assert!(result.is_none(), "普通文本应返回 None");
}

/// 单个 `/` 字符：strip 后为空 → 返回 None
#[tokio::test]
async fn test_intercept_slash_only_returns_none() {
    // Arrange
    let content = MessageContent::text("/");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：单个 `/` 应返回 None（不空命中命令）
    assert!(result.is_none(), "单个 `/` 应返回 None");
}

// ── intercept_immediate_command: Immediate 命令拦截（/clear） ─────────────

/// `/clear` 已迁移到视图层——prompt 路径不再拦截，返回 None（走 agent 管线）
#[tokio::test]
async fn test_intercept_clear_command_not_intercepted_in_prompt_path() {
    // Arrange
    let content = MessageContent::text("/clear");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("你好"), BaseMessage::ai("世界")];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：视图层命令不再被拦截
    assert!(
        result.is_none(),
        "/clear 在 prompt 路径应返回 None（由视图层处理）"
    );
}

/// `/clear` 别名 `/cls` 已迁移到视图层——不再被 prompt 路径拦截
#[tokio::test]
async fn test_intercept_clear_alias_cls_not_intercepted() {
    // Arrange
    let content = MessageContent::text("/cls");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("历史消息")];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：视图层命令不再被拦截
    assert!(
        result.is_none(),
        "/cls 别名在 prompt 路径应返回 None（由视图层处理）"
    );
}

/// `/reset` 别名已迁移到视图层——不再被 prompt 路径拦截
#[tokio::test]
async fn test_intercept_clear_alias_reset_not_intercepted() {
    // Arrange
    let content = MessageContent::text("/reset");
    let history: Vec<BaseMessage> = vec![BaseMessage::ai("对话历史")];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert
    assert!(
        result.is_none(),
        "/reset 别名在 prompt 路径应返回 None（由视图层处理）"
    );
}

// ── intercept_immediate_command: push_done TRAP 验证 ──────────────────────

/// [TRAP] Immediate 命令拦截后必须调用 `push_done`，否则 TUI 永久 loading
/// （issue_2026-05-29-immediate-command-missing-push-done）
#[tokio::test]
async fn test_intercept_compact_command_calls_push_done() {
    // Arrange
    let content = MessageContent::text("/compact");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    intercept_immediate_command(req).await;

    // Assert：必须调用 push_done 一次
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "Immediate 命令拦截后必须调用 push_done（TRAP: TUI 永久 loading）"
    );
}

/// 未拦截路径不应调用 push_done（push_done 由后续 pump 负责）
#[tokio::test]
async fn test_intercept_no_match_does_not_call_push_done() {
    // Arrange
    let content = MessageContent::text("普通文本");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    intercept_immediate_command(req).await;

    // Assert：未拦截时 push_done 为 0（由后续 pump 负责）
    assert_eq!(
        mock_sink.push_done_count(),
        0,
        "未拦截路径不应调用 push_done"
    );
}

// ── intercept_immediate_command: cancel 路径验证 ──────────────────────────

/// cancel 信号已触发时：intercept 仍返回 Some（已拦截），且必然调用 push_done。
///
/// 注意：tokio::select! 对已 ready 的 cancel 和快速完成的命令执行是竞速关系，
/// 对瞬时命令（如 /compact）执行分支可能先完成。本测试只验证不变量：
/// 无论哪个分支执行，push_done 都被调用、结果非 None。
#[tokio::test]
async fn test_intercept_with_cancelled_token_still_returns_some() {
    // Arrange
    let content = MessageContent::text("/compact");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("hello"), BaseMessage::ai("world")];
    let cancel = AgentCancellationToken::new();
    // 预先 cancel，与命令执行竞速
    cancel.cancel();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：无论 select 走哪个分支，结果都应非 None（命令已拦截或被取消）
    assert!(result.is_some(), "已 cancel 的拦截路径仍应返回 Some");
    // 不变量：push_done 必被调用（TRAP 守护）
    assert!(
        mock_sink.push_done_count() >= 1,
        "无论 cancel 还是执行分支，push_done 必被调用至少一次"
    );
}

// ── intercept_immediate_command: recall_items 验证 ─────────────────────────

/// Immediate 命令拦截：recall_items 必须为空（命令不产生 recall）
#[tokio::test]
async fn test_intercept_immediate_returns_empty_recall_items() {
    // Arrange
    let content = MessageContent::text("/compact");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：recall_items 必须为空
    let prompt_result = result.unwrap();
    assert!(
        prompt_result.recall_items.is_empty(),
        "Immediate 命令不应产生 recall items"
    );
}

// ── intercept_immediate_command: ok 字段恒为 true 验证 ────────────────────

/// Immediate 命令拦截：ok 字段恒为 true（命令成功 = agent 不构建 = ok）
#[tokio::test]
async fn test_intercept_immediate_ok_always_true() {
    // Arrange
    let content = MessageContent::text("/compact");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let peri_config: Arc<PeriConfig> = Arc::new(Default::default());
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &peri_config,
        &sink,
        &bg_tx,
        &bg_reg,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert
    let prompt_result = result.unwrap();
    assert!(
        prompt_result.ok,
        "Immediate 命令拦截后 ok 必须为 true（命令成功 = agent 不构建）"
    );
}

// ── is_keepgoing: 跨层判空契约测试 ───────────────────────────────────────
//
// 与 peri-agent stages_test 的
// `test_append_messages_empty_prompt_skipped` / `test_append_messages_whitespace_prompt_kept`
// 成对，双侧锁定 ARC-KEEPGOING-001 的判空语义（`MessageContent::is_empty()`）。

/// 空文本 → keepgoing（TUI keepgoing 按钮的真实 payload 是 `text("")`）
#[test]
fn test_is_keepgoing_empty_text() {
    assert!(is_keepgoing(&MessageContent::text("")));
}

/// 纯空白文本不算空 content block → 非 keepgoing（用户输入空格应正常跑 loop）
#[test]
fn test_is_keepgoing_whitespace_text_not_keepgoing() {
    assert!(!is_keepgoing(&MessageContent::text("   ")));
}

/// 纯附件消息（Blocks([Image])）不是空 → 非 keepgoing（trim 判空会把图片误判）
#[test]
fn test_is_keepgoing_image_block_not_keepgoing() {
    let content = MessageContent::blocks(vec![ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_string(),
            data: "fake".to_string(),
        },
    }]);
    assert!(!is_keepgoing(&content));
}

// ── run_session_loop: keepgoing 短路路径 TRAP 验证 ────────────────────────

/// [TRAP] keepgoing 短路路径（空历史 + 空 prompt）必须调用 `push_done`，
/// 否则 TUI 依赖 AgentDone→TurnDone 退出 loading 的机制失效，界面永久卡在
/// loading（ARC-EVENT-001 / ARC-KEEPGOING-001）。
#[tokio::test]
async fn test_run_session_loop_keepgoing_short_circuit_calls_push_done() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let ctx = make_session_context("test-session");
    let turn = TurnInput {
        event_sink: Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        content: MessageContent::text(""),
        frozen: None,
        history: vec![],
        // keepgoing 语义：不注入 recall（否则 recall 拼进 user 消息使其非空）
        incoming_recalls: vec!["should-be-skipped".to_string()],
        bg_results: vec![],
        langfuse_session: None,
    };

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert
    assert!(result.ok);
    assert_eq!(
        result.stop_reason,
        PromptStopReason::EndTurn,
        "短路返回的 stop_reason 必须为 EndTurn"
    );
    assert!(
        result.recall_items.is_empty(),
        "keepgoing 短路不应产生 recall items"
    );
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "keepgoing 短路路径必须调用 push_done 一次（TRAP: TUI 永久 loading）"
    );
}
