//! executor_helpers.rs 单元测试（L5：自 ACP `executor_test.rs` 随迁）。
//!
//! 重点覆盖 [`intercept_immediate_command`]——命令拦截是 execute_prompt 的
//! 前置短路逻辑，任何回归（如忘记 `push_done`）都会导致 TUI 永久 loading
//! （issue_2026-05-29-immediate-command-missing-push-done）。
//!
//! 随迁适配（R4，断言语义不重写）：`peri_config` 已移出拦截契约——命令
//! 注册表查找经注入的 `command_lookup` 闭包 mock（ACP 协议面注册表语义
//! 由装配面承载）；compact 配置经注入闭包返回默认值。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    command::{AgentCommand, CommandContext, CommandKind, CommandResult, PromptStopReason},
    compact::CompactConfig,
    event::{EventSink, ExecutorEvent},
    messages::{BaseMessage, MessageContent},
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::{intercept_immediate_command, InterceptRequest};

// ── Mock EventSink ─────────────────────────────────────────────────────────

/// Mock EventSink，记录所有 push_done 调用（含 request_id）。
struct MockEventSink {
    push_done_count: Mutex<usize>,
    push_done_request_ids: Mutex<Vec<Option<String>>>,
    push_done_stop_reasons: Mutex<Vec<String>>,
    pushed_events: Mutex<Vec<String>>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            push_done_count: Mutex::new(0),
            push_done_request_ids: Mutex::new(Vec::new()),
            push_done_stop_reasons: Mutex::new(Vec::new()),
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

    async fn push_done(&self, _session_id: &str, stop_reason: &str, request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
        self.push_done_request_ids
            .lock()
            .unwrap()
            .push(request_id.map(String::from));
        self.push_done_stop_reasons
            .lock()
            .unwrap()
            .push(stop_reason.to_string());
    }
}

// ── Fake Immediate 命令（mock command_lookup 注入）──────────────────────────

/// Fake Immediate 命令：execute 返回拦截时的历史（与真实 Immediate 命令的
/// messages 透传语义一致）。
struct FakeImmediateCommand;

#[async_trait]
impl AgentCommand for FakeImmediateCommand {
    fn name(&self) -> &str {
        "compact"
    }

    fn description(&self) -> &str {
        "fake immediate command for tests"
    }

    fn kind(&self) -> CommandKind {
        CommandKind::Immediate
    }

    async fn execute(&self, ctx: CommandContext) -> CommandResult {
        CommandResult {
            messages: ctx.history,
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}

// ── Helper 工厂函数 ─────────────────────────────────────────────────────────

/// 构造最小 InterceptRequest（auxiliary_model / thread_store / frozen 等均为 None）。
///
/// `command_lookup` 为注入的注册表查找 mock（None = 未注册，走 agent 管线；
/// Some = 命令命中，按 kind 决定拦截与否）。
#[allow(clippy::too_many_arguments)]
fn make_intercept_request<'a>(
    content: &'a MessageContent,
    history: &'a [BaseMessage],
    session_id: &'a str,
    cancel: &'a AgentCancellationToken,
    event_sink: &'a Arc<dyn EventSink>,
    bg_event_tx: &'a tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    task_manager: &'a Arc<dyn peri_acp_types::tasks::TaskManager>,
    command_lookup: super::CommandLookupFn,
) -> InterceptRequest<'a> {
    let compact_config_loader: Arc<dyn Fn() -> CompactConfig + Send + Sync> =
        Arc::new(CompactConfig::default);
    InterceptRequest {
        content,
        history,
        cwd: "/tmp",
        session_id,
        cancel,
        thread_store: None,
        thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_system_prompt: None,
        event_sink,
        auxiliary_model: &None,
        bg_event_tx,
        task_manager,
        command_lookup,
        compact_config_loader,
        bg_spawner: None,
    }
}

/// 构造共享的 bg registry + bg channel（拦截测试不实际触发 bg，但需要传入句柄）。
fn make_bg_infra() -> (
    tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    Arc<dyn peri_acp_types::tasks::TaskManager>,
) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(crate::agent::async_tasks::TaskManager::new())
        as Arc<dyn peri_acp_types::tasks::TaskManager>;
    (tx, registry)
}

/// 默认 command_lookup mock：未注册（None），等价 ACP 注册表未命中。
fn no_match_lookup() -> super::CommandLookupFn {
    Arc::new(|_text: &str| None)
}

/// 命中 Fake Immediate 命令的 command_lookup mock（/compact 路径）。
fn immediate_lookup() -> super::CommandLookupFn {
    Arc::new(|text: &str| {
        if text == "compact" {
            Some((
                Arc::new(FakeImmediateCommand) as Arc<dyn AgentCommand>,
                String::new(),
            ))
        } else {
            None
        }
    })
}

// ── intercept_immediate_command: 路径分支测试 ─────────────────────────────

/// 普通 slash 命令（非 Immediate 注册）：不在注入注册表中 → 返回 None
#[tokio::test]
async fn test_intercept_unknown_command_returns_none() {
    // Arrange
    let content = MessageContent::text("/nonexistent");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        no_match_lookup(),
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
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        no_match_lookup(),
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
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        no_match_lookup(),
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
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        no_match_lookup(),
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
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        no_match_lookup(),
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：视图层命令不再被拦截
    assert!(
        result.is_none(),
        "/cls 在 prompt 路径应返回 None（由视图层处理）"
    );
}

/// `/clear` 别名 `/reset` 已迁移到视图层——不再被 prompt 路径拦截
#[tokio::test]
async fn test_intercept_clear_alias_reset_not_intercepted() {
    // Arrange
    let content = MessageContent::text("/reset");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        no_match_lookup(),
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：视图层命令不再被拦截
    assert!(
        result.is_none(),
        "/reset 在 prompt 路径应返回 None（由视图层处理）"
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
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        immediate_lookup(),
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
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        no_match_lookup(),
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
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        immediate_lookup(),
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
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        immediate_lookup(),
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
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        immediate_lookup(),
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert
    let prompt_result = result.unwrap();
    assert!(prompt_result.ok, "Immediate 命令拦截结果 ok 必须为 true");
}
