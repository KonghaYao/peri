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
    thread::{FilesystemThreadStore, ThreadStore},
};

use super::{
    intercept_immediate_command, is_keepgoing, mark_permission_mode_notified,
    permission_mode_notice_if_changed, run_session_loop, FrozenSessionData, InterceptRequest,
    PromptStopReason, SessionContext, TurnInput, PERMISSION_MODE_NEVER_NOTIFIED,
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

/// Mock EventSink，记录所有 push_done 调用（含 request_id）。
struct MockEventSink {
    push_done_count: Mutex<usize>,
    push_done_request_ids: Mutex<Vec<Option<String>>>,
    pushed_events: Mutex<Vec<String>>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            push_done_count: Mutex::new(0),
            push_done_request_ids: Mutex::new(Vec::new()),
            pushed_events: Mutex::new(Vec::new()),
        }
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }

    fn last_push_done_request_id(&self) -> Option<String> {
        self.push_done_request_ids
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
    }
}

#[async_trait]
impl EventSink for MockEventSink {
    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.pushed_events.lock().unwrap().push(json);
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
        self.push_done_request_ids
            .lock()
            .unwrap()
            .push(request_id.map(String::from));
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
        request_id: None,
        allow_await_wake: false,
        v2_event_tx: None,
        continuation_notify: None,
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
        continuation: false,
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

/// Issue 2026-08-05 返工链路验证：keepgoing 短路路径的 push_done 必须透传
/// SessionContext.request_id（服务器回带 → TUI stale TurnInterrupted 配对）。
#[tokio::test]
async fn test_run_session_loop_keepgoing_short_circuit_forwards_request_id() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let mut ctx = make_session_context("test-session");
    ctx.request_id = Some("req-1".to_string());
    let turn = TurnInput {
        event_sink: Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        content: MessageContent::text(""),
        continuation: false,
        frozen: None,
        history: vec![],
        incoming_recalls: vec![],
        bg_results: vec![],
        langfuse_session: None,
    };

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert
    assert!(result.ok);
    assert_eq!(
        mock_sink.last_push_done_request_id().as_deref(),
        Some("req-1"),
        "push_done 必须透传 SessionContext.request_id"
    );
}

// ── run_session_loop: AsyncContinuation 内部续跑（非 keepgoing）─────────────

/// 构造带 SessionManager + 已登记 session 的 SessionContext（可观察 v2 MessageQueue）。
async fn make_session_context_with_manager(
    session_id: &str,
    tmp: &tempfile::TempDir,
) -> (SessionContext, crate::session::SessionManager) {
    let mut ctx = make_session_context(session_id);
    let thread_store =
        Arc::new(FilesystemThreadStore::new(tmp.path().join("threads"))) as Arc<dyn ThreadStore>;
    let mut peri_config = crate::provider::PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![crate::provider::ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: crate::provider::ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config.config.profiles = crate::provider::Profiles {
        sonnet: crate::provider::ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let sm = crate::session::SessionManager::new(
        thread_store,
        LlmProvider::from_config(&peri_config).unwrap(),
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
    );
    sm.new_session_with_id(session_id, "/tmp")
        .await
        .expect("session 登记失败");
    ctx.session_manager = Some(sm.clone());
    (ctx, sm)
}

/// [AsyncContinuation] 内部续跑（continuation=true）不把空 user prompt 当
/// keepgoing：空历史 + 空 prompt 仍进入 agent 管线（绕过 keepgoing 空历史
/// short-circuit——后者会直接返回 ok=true/EndTurn）。
#[tokio::test]
async fn test_continuation_bypasses_keepgoing_short_circuit() {
    // Arrange：预取消 token，保证进入管线后快速中断（不触发真实 LLM 调用）
    let ctx = make_session_context("test-continuation");
    ctx.cancel.cancel();
    let mock_sink = Arc::new(MockEventSink::new());
    let turn = TurnInput {
        event_sink: Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        content: MessageContent::text(""),
        continuation: true,
        frozen: None,
        history: vec![],
        incoming_recalls: vec![],
        bg_results: vec![],
        langfuse_session: None,
    };

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：未走 keepgoing 短路（短路会返回 ok=true/EndTurn 且不构建 agent），
    // 而是进入管线后被预取消 token 中断（ok=false/Cancelled）。
    assert!(!result.ok, "continuation 不得走 keepgoing 空历史短路");
    assert_eq!(
        result.stop_reason,
        PromptStopReason::Cancelled,
        "进入管线后被预取消 token 中断"
    );
}

/// [AsyncContinuation] 内部续跑不写入空 human prompt：Phase 6 跳过 Prompt push，
/// v2 MessageQueue 不出现消息；对比 keepgoing（非空历史）会 push 一条空 Prompt。
#[tokio::test]
async fn test_continuation_skips_empty_prompt_push() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "test-continuation-queue";
    let (ctx, sm) = make_session_context_with_manager(session_id, &tmp).await;
    ctx.cancel.cancel();
    let history = vec![BaseMessage::human("prior turn")];

    // Act 1：continuation=true（空 content + 非空历史）
    let turn = TurnInput {
        event_sink: Arc::new(MockEventSink::new()) as Arc<dyn EventSink>,
        content: MessageContent::text(""),
        continuation: true,
        frozen: None,
        history: history.clone(),
        incoming_recalls: vec![],
        bg_results: vec![],
        langfuse_session: None,
    };
    let _ = run_session_loop(ctx, turn).await;

    // Assert 1：队列无任何消息（未写空 human）
    let queue = sm
        .get_session(session_id)
        .expect("session 应存在")
        .v2_message_queue
        .clone();
    assert!(
        queue.drain_all().is_empty(),
        "continuation 不得向 v2 queue 写入空 human prompt"
    );

    // Act 2：keepgoing（continuation=false，同为空 content）——对比组
    let mut ctx2 = make_session_context(session_id);
    ctx2.session_manager = Some(sm.clone());
    ctx2.cancel.cancel();
    let turn2 = TurnInput {
        event_sink: Arc::new(MockEventSink::new()) as Arc<dyn EventSink>,
        content: MessageContent::text(""),
        continuation: false,
        frozen: None,
        history,
        incoming_recalls: vec![],
        bg_results: vec![],
        langfuse_session: None,
    };
    let _ = run_session_loop(ctx2, turn2).await;

    // Assert 2：keepgoing 会 push 一条 Prompt（空 human 由 stages 跳过转录）
    let drained = sm
        .get_session(session_id)
        .expect("session 应存在")
        .v2_message_queue
        .clone()
        .drain_all();
    assert_eq!(drained.len(), 1, "keepgoing 应 push 一条空 Prompt 消息");
    assert_eq!(
        drained[0].kind,
        peri_agent::session::queue::MessageKind::Prompt,
        "keepgoing push 的消息应为 Prompt kind"
    );
}

// ── FrozenSessionData 前缀稳定性测试 ───────────────────────────────────────

/// [回归测试] 同一 frozen 输入必须产生字节相同的 system prompt。
///
/// 历史背景（ARC-FROZEN-001）：system prompt 在 session/new 时一次性冻结；
/// 相同会话输入（cwd/language/skill roots/date/permission mode）若因调用方
/// 上下文差异产生不同前缀，会破坏 Anthropic 前缀缓存，并使主 agent 与
/// subagent 看到不一致的策略。本测试固定全部输入，验证
/// `FrozenSessionData::build` 是确定性的。
#[test]
fn test_frozen_session_data_build_is_deterministic() {
    let cwd = "/tmp";
    let frozen_date = "2026-01-01";
    let a = FrozenSessionData::build(
        cwd,
        None,
        &[],
        &[],
        frozen_date,
        PermissionMode::Bypass,
        true,
    );
    let b = FrozenSessionData::build(
        cwd,
        None,
        &[],
        &[],
        frozen_date,
        PermissionMode::Bypass,
        true,
    );
    assert_eq!(
        a.system_prompt(),
        b.system_prompt(),
        "相同 frozen 输入两次 build 应产生相同 system prompt"
    );
    assert_eq!(
        a.skill_summary(),
        b.skill_summary(),
        "相同 frozen 输入两次 build 应产生相同 skill 摘要"
    );
}

/// [回归测试] 已冻结的 system prompt 与 skill 摘要不受会话中途磁盘变化影响。
///
/// 历史背景（ARC-FROZEN-001 / 审计 prompt-sections-audit.md P2-11）：skill
/// 摘要与 system prompt 在 session/new 冻结；会话内磁盘 skill 增删不得改变
/// 已冻结产物（冻结是前缀缓存稳定性的有意权衡，不能按需重扫）。
#[test]
fn test_frozen_system_prompt_immune_to_disk_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();

    // 冻结前：cwd 含 skill-a
    let skills_dir_a = tmp.path().join(".claude").join("skills").join("skill-a");
    std::fs::create_dir_all(&skills_dir_a).unwrap();
    std::fs::write(
        skills_dir_a.join("SKILL.md"),
        "---\nname: 'skill-a'\ndescription: 'A test skill'\n---\n\nbody",
    )
    .unwrap();

    let frozen = FrozenSessionData::build(
        cwd,
        None,
        &[],
        &[],
        "2026-01-01",
        PermissionMode::Bypass,
        true,
    );
    let frozen_prompt = frozen.system_prompt().to_string();
    let frozen_summary = frozen.skill_summary().map(|s| s.to_string());
    assert!(
        frozen_summary.as_deref().unwrap_or("").contains("skill-a"),
        "冻结摘要应包含冻结时的 skill-a"
    );

    // 会话中途：删除 skill-a，新增 skill-b 与 CLAUDE.md
    std::fs::remove_dir_all(&skills_dir_a).unwrap();
    let skills_dir_b = tmp.path().join(".claude").join("skills").join("skill-b");
    std::fs::create_dir_all(&skills_dir_b).unwrap();
    std::fs::write(
        skills_dir_b.join("SKILL.md"),
        "---\nname: 'skill-b'\ndescription: 'B test skill'\n---\n\nbody",
    )
    .unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# New CLAUDE.md").unwrap();

    // 已冻结产物不变（不按需重读磁盘）
    assert_eq!(
        frozen.system_prompt(),
        frozen_prompt,
        "已冻结 system prompt 不应受会话中途磁盘变化影响"
    );
    assert_eq!(
        frozen.skill_summary().map(|s| s.to_string()),
        frozen_summary,
        "已冻结 skill 摘要不应随磁盘重扫"
    );
}

/// [回归测试] workflow capability 在 session 冻结时决定 16_workflow section。
///
/// 历史背景（审计 prompt-sections-audit.md P1-5 / 阶段 3 完成判据）：
/// `workflow_executor: None`（-p print mode）时 prompt 不得声明 Workflow；
/// `Some` 时声明存在。`FrozenSessionData::build` 的 workflow_enabled 输入
/// 来自同一条件源（`workflow_executor.is_some()`），与 builder 注册、
/// ToolSearch 发现三面一致。
#[test]
fn test_frozen_prompt_workflow_section_gated_by_workflow_enabled() {
    let cwd = "/tmp";
    let frozen_date = "2026-01-01";

    let enabled = FrozenSessionData::build(
        cwd,
        None,
        &[],
        &[],
        frozen_date,
        PermissionMode::Bypass,
        true,
    );
    assert!(
        enabled.system_prompt().contains("Workflow Orchestration"),
        "workflow_enabled=true 时 16_workflow section 应渲染"
    );

    let disabled = FrozenSessionData::build(
        cwd,
        None,
        &[],
        &[],
        frozen_date,
        PermissionMode::Bypass,
        false,
    );
    assert!(
        !disabled.system_prompt().contains("Workflow Orchestration"),
        "workflow_enabled=false（print mode）时 16_workflow section 不应渲染"
    );
}

/// [回归测试] 子 agent / fork / workflow agent 复用的冻结 prompt 不宣称 workflow。
///
/// 历史背景（P2-2026-08-02 pre-commit review）：`features_for_sub` 与 fork
/// 继承的 parent frozen prompt 会把 16_workflow section 保留为可用，但
/// subagent / fork / workflow agent 三条路径均传 `shared_tools: None`、无
/// WorkflowTool；agent.md 又准确说明不继承 Workflow extension tools，造成
/// prompt 与能力矛盾（system prompt / 工具注册 / SearchExtraTools 三面不一致）。
///
/// 修复后 `FrozenSessionData` 同时冻结主 prompt（workflow 声明，主 ACP/stdio
/// 链真实注册 WorkflowTool）与子面向 prompt（无 16_workflow section）：
/// subagent 的 system_builder、fork/bg-fork 继承、workflow agent 复用的
/// 都是后者；workflow_enabled=false（print mode）时两版字节相同。
#[test]
fn test_frozen_subagent_prompt_never_claims_workflow() {
    let cwd = "/tmp";
    let frozen_date = "2026-01-01";

    // 主链 workflow 可用：主 prompt 声明，子面向 prompt 不声明
    let enabled = FrozenSessionData::build(
        cwd,
        None,
        &[],
        &[],
        frozen_date,
        PermissionMode::Bypass,
        true,
    );
    assert!(
        enabled.system_prompt().contains("Workflow Orchestration"),
        "主链 workflow 可用时主 prompt 应声明（主 ACP/stdio 仍可用）"
    );
    assert!(
        !enabled
            .subagent_system_prompt()
            .contains("Workflow Orchestration"),
        "子 agent / fork / workflow agent 复用的冻结 prompt 不得声明 Workflow"
    );

    // print mode（workflow 不可用）：主 prompt 无 workflow，两版一致
    let disabled = FrozenSessionData::build(
        cwd,
        None,
        &[],
        &[],
        frozen_date,
        PermissionMode::Bypass,
        false,
    );
    assert!(
        !disabled.system_prompt().contains("Workflow Orchestration"),
        "print mode 主 prompt 不应声明 Workflow"
    );
    assert_eq!(
        disabled.subagent_system_prompt(),
        disabled.system_prompt(),
        "workflow 关闭时子面向 prompt 与主 prompt 应字节相同"
    );
}

// ── PermissionMode 切换通知（D2）────────────────────────────────────────────

/// [回归测试] mode 未变化时不产生通知，且不记账。
///
/// 历史背景（审计 prompt-sections-audit.md P1-3）：frozen prompt 呈现初始
/// mode，会话内切换后若不通知模型会产生"prompt 说明陈旧"漂移。D2 采用
/// 下一可消费 turn 的受控 runtime event；检测与记账分离（记账在消息入队时，
/// 见 `mark_permission_mode_notified`），未变化时两者都不发生。
#[test]
fn test_permission_mode_notice_skipped_when_unchanged() {
    let last = std::sync::atomic::AtomicU8::new(PermissionMode::Default as u8);
    let notice = permission_mode_notice_if_changed(PermissionMode::Default, &last);
    assert!(notice.is_none(), "mode 未变化不应产生通知");
    assert_eq!(
        last.load(std::sync::atomic::Ordering::Relaxed),
        PermissionMode::Default as u8,
        "未变化时 last-notified 不应被修改"
    );
}

/// [回归测试] mode 变化时产生受控通知（纯检测，不记账）。
///
/// 通知文本必须包含 mode 名与语义（与 10_hitl.md 的机制说明一致），
/// 且不含保留 tag（由调用方包裹 `<system-reminder>`）。
#[test]
fn test_permission_mode_notice_emitted_on_change() {
    let last = std::sync::atomic::AtomicU8::new(PermissionMode::Default as u8);
    let notice = permission_mode_notice_if_changed(PermissionMode::Bypass, &last);
    let notice = notice.expect("mode 变化应产生通知");
    assert!(notice.contains("Bypass"), "通知应包含 mode 名: {notice}");
    assert!(
        notice.contains("without approval"),
        "通知应包含 mode 语义: {notice}"
    );
    assert!(
        !notice.contains("<system-reminder>"),
        "通知本身不应含保留 tag（由调用方包裹）"
    );
    assert!(
        !notice.contains("Current permission mode"),
        "非首轮变化应使用 'changed to' 措辞，而非初始说明"
    );
    // 纯检测不记账：last-notified 保持原值，直到消息入队（executor_helpers Phase 6）
    assert_eq!(
        last.load(std::sync::atomic::Ordering::Relaxed),
        PermissionMode::Default as u8,
        "检测本身不应更新 last-notified（记账发生在入队点）"
    );
}

/// [回归测试] 记账后不再重复通知：已随消息入队的 mode 变化恰好通知一次。
///
/// 历史背景：通知必须在下一可消费 turn 注入一次；若 CAS 语义错误（每次都
/// 注入），每个后续 turn 都会重复推送陈旧通知，与"切换后通知一次"冲突。
#[test]
fn test_permission_mode_notice_emitted_only_once() {
    let last = std::sync::atomic::AtomicU8::new(PermissionMode::Default as u8);
    // 检测（模拟 run_session_loop 生成文本）→ 入队记账（模拟 Phase 6）
    assert!(permission_mode_notice_if_changed(PermissionMode::AutoMode, &last).is_some());
    mark_permission_mode_notified(&last, PermissionMode::AutoMode);
    assert!(
        permission_mode_notice_if_changed(PermissionMode::AutoMode, &last).is_none(),
        "已记账的 mode 不应重复通知"
    );
    // 再次切换（AutoMode → AcceptEdit → 回到 AutoMode）仍各通知一次
    assert!(permission_mode_notice_if_changed(PermissionMode::AcceptEdit, &last).is_some());
    mark_permission_mode_notified(&last, PermissionMode::AcceptEdit);
    assert!(permission_mode_notice_if_changed(PermissionMode::AutoMode, &last).is_some());
    mark_permission_mode_notified(&last, PermissionMode::AutoMode);
    assert!(
        permission_mode_notice_if_changed(PermissionMode::AutoMode, &last).is_none(),
        "回到 AutoMode 且已记账后不应再次通知"
    );
}

/// [回归测试] 未入队前失败/取消不记账，通知可重复重试（不丢失）。
///
/// 历史背景（P3-2026-08-02 pre-commit review）：旧实现"一生成即写
/// last_notified"，若本 turn 在通知被模型消费前失败或取消，下一 turn 因
/// last-notified 已更新而不再生成通知——通知丢失。修复后检测与记账分离：
/// 只有消息推入模型可见 v2 MessageQueue（executor_helpers Phase 6 入队点）
/// 才记账；入队前失败/取消时，下一 turn 重新检测仍会生成相同通知。
#[test]
fn test_permission_mode_notice_retries_until_enqueued() {
    let last = std::sync::atomic::AtomicU8::new(PermissionMode::Default as u8);

    // turn 1：检测到变化（生成文本），但 turn 在入队前失败/取消 → 不记账
    let n1 = permission_mode_notice_if_changed(PermissionMode::Bypass, &last);
    assert!(n1.is_some(), "turn 1 应检测到 mode 变化");
    assert_eq!(
        last.load(std::sync::atomic::Ordering::Relaxed),
        PermissionMode::Default as u8,
        "入队前失败不应记账"
    );

    // turn 2（重试）：仍能检测到同一变化，通知未丢失
    let n2 = permission_mode_notice_if_changed(PermissionMode::Bypass, &last);
    assert!(n2.is_some(), "重试 turn 应再次生成通知（不丢失）");
    assert_eq!(n1, n2, "重试通知文本应与首次一致");

    // turn 2 消息成功入队 → 记账；turn 3 不再重复
    mark_permission_mode_notified(&last, PermissionMode::Bypass);
    assert!(
        permission_mode_notice_if_changed(PermissionMode::Bypass, &last).is_none(),
        "入队记账后不应再通知"
    );
}

/// [回归测试] 初始 mode 在首个模型可见 turn 恰好公开一次。
///
/// 历史背景（P3-2026-08-02 pre-commit review）：10_hitl 不含 mode snapshot、
/// Bypass 时 10_hitl 不渲染，初始 mode 从不向模型公开。修复后
/// `last_notified_permission_mode` 初始化为 [`PERMISSION_MODE_NEVER_NOTIFIED`]
/// 哨兵：首个模型可见 turn 生成"当前模式"初始说明，入队记账后不再重复；
/// 之后仅对真实 mode 切换生成"changed to"说明。
#[test]
fn test_permission_mode_initial_notice_disclosed_once() {
    let last = std::sync::atomic::AtomicU8::new(PERMISSION_MODE_NEVER_NOTIFIED);

    // 首轮：初始说明（即使 mode 未"切换"，也要公开初始 mode）
    let notice = permission_mode_notice_if_changed(PermissionMode::Default, &last);
    let notice = notice.expect("首轮应公开初始 mode");
    assert!(
        notice.contains("Current permission mode") && notice.contains("Default"),
        "首轮应使用'当前模式'初始说明: {notice}"
    );
    assert!(
        !notice.contains("changed to"),
        "首轮不应使用'changed to'措辞"
    );

    // 入队记账后：同一 mode 不再重复通知
    mark_permission_mode_notified(&last, PermissionMode::Default);
    assert!(
        permission_mode_notice_if_changed(PermissionMode::Default, &last).is_none(),
        "初始说明恰好公开一次"
    );

    // 后续真实切换走 "changed to" 措辞
    let change = permission_mode_notice_if_changed(PermissionMode::Bypass, &last);
    let change = change.expect("切换应产生通知");
    assert!(
        change.contains("changed to") && change.contains("Bypass"),
        "切换应使用 'changed to' 措辞: {change}"
    );
}
