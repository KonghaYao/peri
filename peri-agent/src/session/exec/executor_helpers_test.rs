//! executor_helpers.rs 单元测试（L5：自 ACP `executor_test.rs` 随迁）。
//!
//! 重点覆盖 [`intercept_immediate_command`]——命令拦截是 execute_prompt 的
//! 前置短路逻辑，任何回归（如忘记 `push_done`）都会导致 TUI 永久 loading
//! （issue_2026-05-29-immediate-command-missing-push-done）。
//!
//! 随迁适配（R4，断言语义不重写）：`peri_config` 已移出拦截契约——命令
//! 注册表查找经注入的 `command_lookup` 闭包 mock（ACP 协议面注册表语义
//! 由装配面承载，返回 `ResolvedCommand`，假 handler 执行）；
//! compact 配置经注入闭包返回默认值。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    command::{
        command_handler::CommandHandler, command_route::RouteEntry, CommandContext,
        CommandFeedback, CommandOutcome, CommandResult, FeedbackChannel, FeedbackLevel,
        PromptStopReason, ResolvedCommand,
    },
    compact::CompactConfig,
    event::{EventSink, ExecutorEvent},
    messages::{BaseMessage, MessageContent},
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::{
    emit_command_feedback, intercept_immediate_command, InterceptOutcome, InterceptRequest,
};

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

// ── Fake CommandHandler（mock command_lookup 注入）─────────────────────────

/// Fake CommandHandler：execute 返回拦截时的历史（与真实 Immediate 命令的
/// messages 透传语义一致）。
struct FakeImmediateHandler;

#[async_trait]
impl CommandHandler for FakeImmediateHandler {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(CommandResult {
            messages: ctx.history,
            stop_reason: PromptStopReason::EndTurn,
            feedback: None,
        })
    }
}

/// 测试用 RouteEntry（core 域 Command 条目，假 handler）。
fn test_route_entry() -> RouteEntry {
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource,
    };
    RouteEntry {
        fullname: "core:compact".to_string(),
        aliases: vec![],
        description: "fake immediate command for tests".to_string(),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema: None,
        handler: Arc::new(FakeImmediateHandler),
        provenance: CommandProvenance {
            source: CommandSource::Core,
            lifecycle: CommandLifecycle::Connected,
        },
    }
}

// ── Helper 工厂函数 ─────────────────────────────────────────────────────────

/// 构造最小 InterceptRequest（auxiliary_model / thread_store / frozen 等均为 None）。
///
/// `command_lookup` 为注入的注册表查找 mock（None = 未注册，走 agent 管线；
/// Some = 命中，执行由 CommandOutcome 承载）。
#[allow(clippy::too_many_arguments)]
fn make_intercept_request<'a>(
    content: &'a MessageContent,
    history: &'a [BaseMessage],
    session_id: &'a str,
    cancel: &'a AgentCancellationToken,
    event_sink: &'a Arc<dyn EventSink>,
    _bg_event_tx: &'a tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
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
        task_manager,
        command_lookup,
        compact_config_loader,
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

/// 命中 Fake CommandHandler 的 command_lookup mock（/compact 路径；
/// P1-6：返回 `ResolvedCommand`，args 词法切分由注册表 resolve 完成）。
fn immediate_lookup() -> super::CommandLookupFn {
    Arc::new(|text: &str| {
        if text == "compact" {
            Some(ResolvedCommand {
                entry: Arc::new(test_route_entry()),
                args: String::new(),
            })
        } else {
            None
        }
    })
}

/// 命中返回 Inject 的 handler 的 command_lookup mock（Phase 5 Step 6 语义：
/// 回传 `Inject(text)`，executor.rs 调用点转 AgentInput::blocks 进 agent 管线）。
fn inject_lookup() -> super::CommandLookupFn {
    struct InjectHandler;
    #[async_trait]
    impl CommandHandler for InjectHandler {
        async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
            CommandOutcome::Inject("/skill tdd".to_string())
        }
    }
    Arc::new(|text: &str| {
        if text == "inject-me" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:inject-me".to_string(),
                    aliases: vec![],
                    description: "inject handler for tests".to_string(),
                    kind: peri_acp_types::command::command_route::CommandEntryKind::Command,
                    category: None,
                    args_schema: None,
                    handler: Arc::new(InjectHandler),
                    provenance: peri_acp_types::command::command_route::CommandProvenance {
                        source: peri_acp_types::command::command_route::CommandSource::Core,
                        lifecycle:
                            peri_acp_types::command::command_route::CommandLifecycle::Connected,
                    },
                }),
                args: String::new(),
            })
        } else {
            None
        }
    })
}

// ── intercept_immediate_command: 路径分支测试 ─────────────────────────────

/// 普通 slash 命令（非 Immediate 注册）：不在注入注册表中 → PassThrough
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

    // Assert：未知命令不拦截，PassThrough 继续走 agent 管线
    assert!(
        matches!(result, InterceptOutcome::PassThrough),
        "未知命令应 PassThrough 继续走 agent 管线"
    );
}

/// 普通文本（无 `/` 前缀）：PassThrough
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
    assert!(
        matches!(result, InterceptOutcome::PassThrough),
        "普通文本应 PassThrough"
    );
}

/// 单个 `/` 字符：strip 后为空 → PassThrough
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

    // Assert：单个 `/` 应 PassThrough（不空命中命令）
    assert!(
        matches!(result, InterceptOutcome::PassThrough),
        "单个 `/` 应 PassThrough"
    );
}

/// `/etc/hosts` 绝对路径输入：strip 后注册表未命中 → PassThrough（fall through
/// 进 agent 管线，不产生错误事件、不硬报错——设计 §78 未解析一律 fall through）。
#[tokio::test]
async fn test_intercept_etc_hosts_falls_through() {
    // Arrange
    let content = MessageContent::text("/etc/hosts");
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
    let result = intercept_immediate_command(req).await;

    // Assert：绝对路径未命中注册表 → PassThrough，且不产生任何事件
    assert!(
        matches!(result, InterceptOutcome::PassThrough),
        "/etc/hosts 应 PassThrough 走 agent 管线"
    );
    assert!(
        mock_sink.pushed_events.lock().unwrap().is_empty(),
        "fall through 路径不得产生错误事件"
    );
    assert_eq!(
        mock_sink.push_done_count(),
        0,
        "fall through 路径不得 push_done"
    );
}

/// `mcp__demo__hello`（废弃 mcp__ 双下划线形态，无 `/` 前缀）：未命中 →
/// PassThrough（fall through 进 agent 管线，不硬报错——词法层拒绝是注册表
/// resolve 的职责，拦截层不产生错误事件）。
#[tokio::test]
async fn test_intercept_mcp_legacy_form_falls_through() {
    // Arrange
    let content = MessageContent::text("mcp__demo__hello");
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
    let result = intercept_immediate_command(req).await;

    // Assert：mcp__ 遗留形态不拦截、不报错 → PassThrough
    assert!(
        matches!(result, InterceptOutcome::PassThrough),
        "mcp__ 遗留形态应 PassThrough 走 agent 管线"
    );
    assert!(
        mock_sink.pushed_events.lock().unwrap().is_empty(),
        "fall through 路径不得产生错误事件"
    );
    assert_eq!(
        mock_sink.push_done_count(),
        0,
        "fall through 路径不得 push_done"
    );
}

// ── intercept_immediate_command: Immediate 命令拦截（/clear 注册表命中） ─────

/// 命中 Fake ClearHandler 的 command_lookup mock（core:clear 注册形态：
/// 别名 cls/reset，args_schema = ArgsSchema::default() 零校验；语义与真实
/// ClearCommand 对齐——messages 清空 + feedback(Info, "对话已清空", UiOnly)）。
fn clear_lookup() -> super::CommandLookupFn {
    struct FakeClearHandler;
    #[async_trait]
    impl CommandHandler for FakeClearHandler {
        async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
            CommandOutcome::Done(CommandResult {
                messages: Vec::new(), // 语义保持：清空后会话为空
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Info,
                    message: "对话已清空".to_string(),
                    channel: FeedbackChannel::UiOnly,
                }),
            })
        }
    }
    Arc::new(|text: &str| {
        matches!(text, "clear" | "cls" | "reset").then(|| ResolvedCommand {
            entry: Arc::new(RouteEntry {
                fullname: "core:clear".to_string(),
                aliases: vec!["cls".to_string(), "reset".to_string()],
                description: "清空当前会话的对话历史".to_string(),
                kind: peri_acp_types::command::command_route::CommandEntryKind::Command,
                category: None,
                args_schema: Some(peri_acp_types::command::ArgsSchema::default()),
                handler: Arc::new(FakeClearHandler),
                provenance: peri_acp_types::command::command_route::CommandProvenance {
                    source: peri_acp_types::command::command_route::CommandSource::Core,
                    lifecycle: peri_acp_types::command::command_route::CommandLifecycle::Connected,
                },
            }),
            args: String::new(),
        })
    })
}

/// 断言 clear 拦截结果：Handled + messages 清空 + CommandFeedback
/// (Info, "对话已清空", UiOnly) + push_done 恰好一次。
fn assert_clear_handled(result: InterceptOutcome, mock_sink: &MockEventSink) {
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("clear 注册表命中应返回 Handled");
    };
    assert!(prompt_result.ok);
    assert_eq!(prompt_result.stop_reason, PromptStopReason::EndTurn);
    assert!(
        prompt_result.messages.is_empty(),
        "clear 应清空消息历史（真实 ClearCommand messages: Vec::new() 语义）"
    );
    let events = mock_sink.pushed_events.lock().unwrap();
    let fb_event = events.iter().find(|json| json.contains("command_feedback"));
    assert!(fb_event.is_some(), "clear 应发射 CommandFeedback 事件");
    let fb_json = fb_event.unwrap();
    assert!(
        fb_json.contains("对话已清空") && fb_json.contains("uiOnly"),
        "反馈应为 feedback(Info, 对话已清空, UiOnly)，实际: {fb_json}"
    );
    drop(events);
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "clear 拦截必须调用 push_done（TRAP: TUI 永久 loading）"
    );
}

/// `/clear` 已注册进 ACP 注册表（core:clear，Phase 5 Step 3）——prompt 路径
/// 经 command_lookup resolve 命中 → 拦截层确定性执行清空（不再 PassThrough
/// fall-through）。
#[tokio::test]
async fn test_intercept_clear_hits_registry_and_handles() {
    // Arrange
    let content = MessageContent::text("/clear");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("你好"), BaseMessage::ai("世界")];
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
        clear_lookup(),
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：注册表命中 → 确定性清空
    assert_clear_handled(result, &mock_sink);
}

/// `/clear` 别名 `/cls`：注册表 alias 索引命中 → 同样确定性清空。
#[tokio::test]
async fn test_intercept_clear_alias_cls_hits_registry_and_handles() {
    // Arrange
    let content = MessageContent::text("/cls");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("历史消息")];
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
        clear_lookup(),
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：别名命中 → 确定性清空
    assert_clear_handled(result, &mock_sink);
}

/// `/clear` 别名 `/reset`：注册表 alias 索引命中 → 同样确定性清空。
#[tokio::test]
async fn test_intercept_clear_alias_reset_hits_registry_and_handles() {
    // Arrange
    let content = MessageContent::text("/reset");
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
        clear_lookup(),
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：别名命中 → 确定性清空
    assert_clear_handled(result, &mock_sink);
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

    // Assert：无论 select 走哪个分支，结果都应 Handled（命令已拦截或被取消）
    assert!(
        matches!(result, InterceptOutcome::Handled(_)),
        "已 cancel 的拦截路径仍应返回 Handled"
    );
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
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("Immediate 命令拦截应返回 Handled");
    };
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
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("Immediate 命令拦截应返回 Handled");
    };
    assert!(prompt_result.ok, "Immediate 命令拦截结果 ok 必须为 true");
}

// ── intercept_immediate_command: Inject / args 解析 / cancel 分发验证 ───────

/// handler 返回 Inject：回传 Inject(text)（不 push_done——agent pump 负责，
/// executor.rs 调用点转 AgentInput::blocks(text) 进 agent 管线）。
#[tokio::test]
async fn test_intercept_inject_outcome_returns_inject() {
    // Arrange
    let content = MessageContent::text("/inject-me");
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
        inject_lookup(),
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：Inject 原样回传（注入文本进 agent 管线），不 push_done
    assert!(
        matches!(result, InterceptOutcome::Inject(ref text) if text == "/skill tdd"),
        "Inject 应原样回传注入文本"
    );
    assert_eq!(
        mock_sink.push_done_count(),
        0,
        "Inject 路径不应调用 push_done（由 agent pump 负责）"
    );
}

/// [回归测试] AgentPassthrough 行为镜像（core 域 skill 条目占位 handler）：
/// 命中 skill 命令时 `Inject(ctx.raw_text)`——用户消息原文整段（含
/// `/skill-name` token）回传 agent 管线，SkillPreload 中间件自动检测分支
/// 依赖原文，命令不被吞。
///
/// 历史背景：Phase 5 Step 6 拦截层删除 `kind != Immediate` fall-through
/// 守卫后，skill 条目经 AgentPassthrough 执行，占位实现返回空串 Inject，
/// 用户消息被整体替换为空文本，skill 预加载失效（2026-08-16）。
#[tokio::test]
async fn test_intercept_skill_passthrough_injects_original_text() {
    struct PassthroughHandler;
    #[async_trait]
    impl CommandHandler for PassthroughHandler {
        async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
            CommandOutcome::Inject(ctx.raw_text)
        }
    }
    let lookup: super::CommandLookupFn = Arc::new(|text: &str| {
        // 镜像注册表 resolve 语义：整段文本进入，名字 + args 词法切分完成。
        if text == "diagnose 帮我调试一下" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:diagnose".to_string(),
                    aliases: vec![],
                    description: "test skill".to_string(),
                    kind: peri_acp_types::command::command_route::CommandEntryKind::Skill,
                    category: None,
                    args_schema: None,
                    handler: Arc::new(PassthroughHandler),
                    provenance: peri_acp_types::command::command_route::CommandProvenance {
                        source: peri_acp_types::command::command_route::CommandSource::Core,
                        lifecycle:
                            peri_acp_types::command::command_route::CommandLifecycle::Connected,
                    },
                }),
                args: "帮我调试一下".to_string(),
            })
        } else {
            None
        }
    });

    let content = MessageContent::text("/diagnose 帮我调试一下");
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
        lookup,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：skill 命中 → Inject 回传原文（含 `/skill-name` token），不 push_done
    assert!(
        matches!(result, InterceptOutcome::Inject(ref text) if text == "/diagnose 帮我调试一下"),
        "skill 命中应将原文整段回传 agent 管线（原文被吞 = skill 预加载失效）"
    );
    assert_eq!(
        mock_sink.push_done_count(),
        0,
        "Inject 路径不应调用 push_done（由 agent pump 负责）"
    );
}

/// 新增：cancel 分支（外层 cancel 已触发）→ Handled(Cancelled) + push_done，
/// messages = history 原样（cancel 语义保持）。
#[tokio::test]
async fn test_intercept_cancel_outcome_returns_handled_cancelled() {
    // Arrange：预先 cancel；handler 挂一个永不返回的假 handler 保证 select
    // 必然走 cancel 分支（与 PendingEventSink 同理——瞬时命令可能先完成）。
    struct PendingHandler;
    #[async_trait]
    impl CommandHandler for PendingHandler {
        async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
            std::future::pending::<()>().await;
            unreachable!("pending 永不返回");
        }
    }
    let lookup: super::CommandLookupFn = Arc::new(|text: &str| {
        if text == "pending" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:pending".to_string(),
                    aliases: vec![],
                    description: "pending handler".to_string(),
                    kind: peri_acp_types::command::command_route::CommandEntryKind::Command,
                    category: None,
                    args_schema: None,
                    handler: Arc::new(PendingHandler),
                    provenance: peri_acp_types::command::command_route::CommandProvenance {
                        source: peri_acp_types::command::command_route::CommandSource::Core,
                        lifecycle:
                            peri_acp_types::command::command_route::CommandLifecycle::Connected,
                    },
                }),
                args: String::new(),
            })
        } else {
            None
        }
    });

    let content = MessageContent::text("/pending");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("hello")];
    let cancel = AgentCancellationToken::new();
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
        lookup,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：cancel 分支 → Handled(Cancelled) + history 原样 + push_done
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("cancel 分支应返回 Handled");
    };
    assert_eq!(prompt_result.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(prompt_result.messages.len(), 1, "cancel 应返回原样 history");
    assert!(prompt_result.ok);
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "cancel 分支必须调用 push_done（TRAP 守护）"
    );
}

/// args 解析失败：schema 声明 required positional，args 缺失 → 不进入 handler，
/// 立即返回 Handled + feedback(Error, 解析失败) + history 原样 + push_done。
#[tokio::test]
async fn test_intercept_args_parse_failure_returns_error_feedback() {
    // Arrange：rewind 形态 schema（required positional + flag）；handler 为
    // 哨兵——若被调用即 panic（解析失败必须不进入 handler）。
    struct SentryHandler;
    #[async_trait]
    impl CommandHandler for SentryHandler {
        async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
            panic!("解析失败路径不得进入 handler");
        }
    }
    let schema = peri_acp_types::command::ArgsSchema {
        positionals: vec![peri_acp_types::command::ArgSpec {
            name: "target_message_id".into(),
            kind: peri_acp_types::command::ArgKind::String,
            required: true,
            description: None,
        }],
        named: vec![],
        flags: vec![peri_acp_types::command::FlagSpec {
            name: "no-revert-files".into(),
            short: None,
            description: None,
        }],
    };
    let lookup: super::CommandLookupFn = Arc::new(move |text: &str| {
        if text == "rewind" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:rewind".to_string(),
                    aliases: vec![],
                    description: "rewind for args-parse test".to_string(),
                    kind: peri_acp_types::command::command_route::CommandEntryKind::Command,
                    category: None,
                    args_schema: Some(schema.clone()),
                    handler: Arc::new(SentryHandler),
                    provenance: peri_acp_types::command::command_route::CommandProvenance {
                        source: peri_acp_types::command::command_route::CommandSource::Core,
                        lifecycle:
                            peri_acp_types::command::command_route::CommandLifecycle::Connected,
                    },
                }),
                args: String::new(),
            })
        } else {
            None
        }
    });

    let content = MessageContent::text("/rewind");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("hello")];
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
        lookup,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：Handled + feedback(Error, 参数解析失败) + history 原样 + push_done
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("解析失败应返回 Handled");
    };
    assert!(prompt_result.ok);
    assert_eq!(prompt_result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(
        prompt_result.messages.len(),
        1,
        "解析失败应返回原样 history"
    );
    // feedback 经 emit_command_feedback 发射为 CommandFeedback 事件
    let events = mock_sink.pushed_events.lock().unwrap();
    let fb_event = events.iter().find(|json| json.contains("command_feedback"));
    assert!(fb_event.is_some(), "解析失败应发射 CommandFeedback 事件");
    assert!(
        fb_event.unwrap().contains("rewind 参数解析失败"),
        "错误消息应含 'rewind 参数解析失败'，实际: {events:?}"
    );
    drop(events);
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "解析失败路径必须调用 push_done（TRAP 守护）"
    );
}

/// args 解析通过：schema 声明 required positional，args 提供 → 正常进入
/// handler（SentryHandler 替换为正常 handler）。
#[tokio::test]
async fn test_intercept_args_parse_ok_passes_into_handler() {
    // Arrange：rewind 形态 schema + 正常 handler（Done + history 原样）
    struct OkHandler;
    #[async_trait]
    impl CommandHandler for OkHandler {
        async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
            assert_eq!(
                ctx.args, "abc123 --no-revert-files",
                "ctx.args 应为 resolve 切分原文"
            );
            // P1-1：统一解析结果经 ctx.parsed_args 传入——handler 不再自研解析
            let parsed = ctx
                .parsed_args
                .as_ref()
                .expect("解析通过路径应携带 parsed_args");
            assert_eq!(
                parsed.positionals,
                vec!["abc123".to_string()],
                "positionals[0] 应为 target_message_id"
            );
            assert_eq!(
                parsed.flags,
                vec!["no-revert-files".to_string()],
                "flags 应命中 no-revert-files"
            );
            CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: None,
            })
        }
    }
    let schema = peri_acp_types::command::ArgsSchema {
        positionals: vec![peri_acp_types::command::ArgSpec {
            name: "target_message_id".into(),
            kind: peri_acp_types::command::ArgKind::String,
            required: true,
            description: None,
        }],
        named: vec![],
        flags: vec![peri_acp_types::command::FlagSpec {
            name: "no-revert-files".into(),
            short: None,
            description: None,
        }],
    };
    let lookup: super::CommandLookupFn = Arc::new(move |text: &str| {
        if text.starts_with("rewind") {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:rewind".to_string(),
                    aliases: vec![],
                    description: "rewind for args-parse test".to_string(),
                    kind: peri_acp_types::command::command_route::CommandEntryKind::Command,
                    category: None,
                    args_schema: Some(schema.clone()),
                    handler: Arc::new(OkHandler),
                    provenance: peri_acp_types::command::command_route::CommandProvenance {
                        source: peri_acp_types::command::command_route::CommandSource::Core,
                        lifecycle:
                            peri_acp_types::command::command_route::CommandLifecycle::Connected,
                    },
                }),
                // resolve 词法切分（不变式 3）：命令名后的参数原样
                args: "abc123 --no-revert-files".to_string(),
            })
        } else {
            None
        }
    });

    let content = MessageContent::text("/rewind abc123 --no-revert-files");
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
        lookup,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：Handled + handler 已执行（OkHandler 内断言 ctx.args）
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("解析通过应返回 Handled");
    };
    assert!(prompt_result.ok);
    assert_eq!(mock_sink.push_done_count(), 1, "解析通过路径必须 push_done");
}

// ── emit_command_feedback: 反馈双通道验证 ───────────────────────────────────

/// 构造带 feedback 的 CommandResult（messages 预置一条 human 消息）。
fn result_with_feedback(channel: FeedbackChannel) -> CommandResult {
    CommandResult {
        messages: vec![BaseMessage::human("你好")],
        stop_reason: PromptStopReason::EndTurn,
        feedback: Some(CommandFeedback {
            level: FeedbackLevel::Info,
            message: "命令已完成".to_string(),
            channel,
        }),
    }
}

/// channel=Session：message 以系统消息追加进 messages 尾部，事件发射一次
/// （Step 1：编排层统一反馈出口；Session 仅命令显式 opt-in，设计 §79）。
#[tokio::test]
async fn test_emit_command_feedback_session_appends_system_message() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let mut result = result_with_feedback(FeedbackChannel::Session);

    // Act
    emit_command_feedback(&sink, "test-session", &mut result).await;

    // Assert：尾部为系统消息（内容同 feedback.message）
    let messages = &result.messages;
    assert_eq!(messages.len(), 2, "Session 通道应追加一条系统消息");
    let last = messages.last().unwrap();
    assert!(
        matches!(last, BaseMessage::System { .. }),
        "尾元素应为系统消息"
    );
    assert_eq!(last.content(), "命令已完成");
    // feedback 已被 take（发射唯一归属本 helper），事件发射一次
    assert!(result.feedback.is_none(), "feedback 应被 take 出");
    assert_eq!(
        mock_sink.pushed_events.lock().unwrap().len(),
        1,
        "Session 通道也应发射 CommandFeedback 事件"
    );
}

/// channel=UiOnly：messages 不变（不追加系统消息），事件仍发射
#[tokio::test]
async fn test_emit_command_feedback_ui_only_keeps_messages() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let mut result = result_with_feedback(FeedbackChannel::UiOnly);

    // Act
    emit_command_feedback(&sink, "test-session", &mut result).await;

    // Assert：messages 不变（UiOnly 不进会话，设计 §79）
    assert_eq!(result.messages.len(), 1, "UiOnly 不应追加消息");
    assert!(result.feedback.is_none(), "feedback 应被 take 出");
    assert_eq!(
        mock_sink.pushed_events.lock().unwrap().len(),
        1,
        "UiOnly 仍应发射 CommandFeedback 事件"
    );
}

// ── LoopResult → ExecOutcome 分类映射（v2 Phase 9）─────────────────────────
//
// spec/issues/2026-08-18-acp-error-handler.md Commit 1：fatal / cancel /
// max-iterations 三类不会混淆，且 fatal 的 public message 非空并脱敏。
mod loop_result_mapping {
    use peri_acp_types::{
        command::PromptStopReason,
        error::AgentError,
        session::{ExecutionFailureKind, EXECUTION_FAILURE_FALLBACK_MESSAGE},
    };

    use crate::agent::stages::LoopResult;

    use super::super::v2_execute::map_loop_result_to_outcome;

    /// 真正 fatal（LLM 错误）：ok=false + failure=Some(Internal, 非空脱敏)。
    /// 消息来自 `user_facing_message()`，不得泄露 provider body / secret。
    #[test]
    fn fatal_llm_error_maps_to_internal_failure() {
        let (ok, stop_reason, failure) = map_loop_result_to_outcome(
            &LoopResult::Error(AgentError::LlmError(
                "provider 500: Authorization: Bearer top-secret-key".to_string(),
            )),
            false,
        );
        assert!(!ok);
        assert_eq!(stop_reason, PromptStopReason::EndTurn);
        let failure = failure.expect("fatal error 必须携带 failure");
        assert_eq!(failure.kind, ExecutionFailureKind::Internal);
        assert!(
            !failure.public_message.is_empty(),
            "public message 必须非空"
        );
        // 脱敏：内部错误/LLM 错误只暴露通用文案
        assert!(!failure.public_message.contains("top-secret-key"));
        assert!(!failure.public_message.contains("Bearer"));
        assert!(failure.public_message.contains("LLM API error"));
    }

    /// 用户主动中断：failure=None，stop_reason=Cancelled。
    #[test]
    fn interrupted_maps_to_no_failure() {
        let (ok, stop_reason, failure) =
            map_loop_result_to_outcome(&LoopResult::Interrupted, false);
        assert!(!ok);
        assert_eq!(stop_reason, PromptStopReason::Cancelled);
        assert!(failure.is_none(), "Interrupted 不得升级为 fatal failure");
    }

    /// cancel token 已取消（即便 Error 非 Interrupted）：failure=None，
    /// 视为用户取消而非请求失败。
    #[test]
    fn cancelled_error_maps_to_no_failure() {
        let (ok, stop_reason, failure) = map_loop_result_to_outcome(
            &LoopResult::Error(AgentError::LlmError("cancelled while failing".to_string())),
            true,
        );
        assert!(!ok);
        assert_eq!(stop_reason, PromptStopReason::Cancelled);
        assert!(failure.is_none(), "用户 cancel 不得升级为 fatal failure");
    }

    /// 最大轮数：failure=None，stop_reason=MaxTurnRequests。
    #[test]
    fn max_iterations_maps_to_no_failure() {
        let (ok, stop_reason, failure) = map_loop_result_to_outcome(
            &LoopResult::Error(AgentError::MaxIterationsExceeded(500)),
            false,
        );
        assert!(!ok);
        assert_eq!(stop_reason, PromptStopReason::MaxTurnRequests);
        assert!(
            failure.is_none(),
            "MaxIterationsExceeded 不得升级为 fatal failure"
        );
    }

    /// 正常完成：ok=true + failure=None。
    #[test]
    fn completed_maps_to_success_no_failure() {
        let (ok, stop_reason, failure) = map_loop_result_to_outcome(&LoopResult::Completed, false);
        assert!(ok);
        assert_eq!(stop_reason, PromptStopReason::EndTurn);
        assert!(failure.is_none());
    }

    /// 其它 fatal（如 LLM HTTP 错误）：failure=Some，public message 为
    /// `user_facing_message()` 的脱敏通用文案（不泄露 provider body）。
    #[test]
    fn llm_http_error_maps_to_sanitized_message() {
        let (ok, _stop_reason, failure) = map_loop_result_to_outcome(
            &LoopResult::Error(AgentError::LlmHttpError {
                status: 500,
                message: "secret-provider-body".to_string(),
            }),
            false,
        );
        assert!(!ok);
        let failure = failure.expect("fatal error 必须携带 failure");
        assert_eq!(failure.kind, ExecutionFailureKind::Internal);
        assert!(!failure.public_message.is_empty());
        assert!(
            !failure.public_message.contains("secret-provider-body"),
            "不得泄露 provider body"
        );
        assert!(failure.public_message.contains("LLM API error"));
    }

    /// 脱敏契约：任何 fatal failure 的 public message 都不得为空，
    /// fallback 文案本身不含内部细节。
    #[test]
    fn fallback_message_is_non_empty_and_safe() {
        assert!(!EXECUTION_FAILURE_FALLBACK_MESSAGE.is_empty());
        assert!(!EXECUTION_FAILURE_FALLBACK_MESSAGE.contains("secret"));
    }
}
