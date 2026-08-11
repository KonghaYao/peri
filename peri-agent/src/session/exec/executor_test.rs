//! executor.rs 单元测试（L5：自 `peri-acp/src/host/exec/executor_test.rs` 随迁）。
//!
//! 归属说明：keepgoing 判定三例 + keepgoing 短路 push_done（TRAP）+
//! request_id 透传 + PermissionMode 通知（纯函数）随 `run_session_loop` 迁入
//! 本 crate（ARC-KEEPGOING-001 契约测试）；完整装配路径测试
//! （continuation / turn 终态唯一 / frozen 渲染）留在 ACP 宿主侧
//! （`host/executor_flow_test.rs`——stage 装配注入面在 ACP，测试
//! 经宿主构造点注入真实 stage_build 桥）。
//!
//! Mock 命名遵循 CLAUDE.md：`make_` 前缀（函数），`Mock` 前缀（结构体）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    agents::AgentCapability,
    event::{
        EventMessage, EventPublisher, EventSink, EventSubscriber, ExecutorEvent, SubscriptionError,
    },
    interaction::{InteractionContext, InteractionResponse, UserInteractionBroker},
    messages::{BaseMessage, ContentBlock, ImageSource, MessageContent},
    permission::{PermissionMode, SharedPermissionMode},
    ports::{SkillsPort, ToolSearchPort},
    runtime::UnstampedEvent,
    skills::{SkillMetadata, SkillRoot},
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::{
    is_keepgoing, mark_permission_mode_notified, permission_mode_notice_if_changed,
    run_session_loop, PromptStopReason, SessionContext, TurnInput, PERMISSION_MODE_NEVER_NOTIFIED,
};
use crate::{
    middleware::MiddlewareChain,
    session::{
        exec::executor_helpers::{ForwarderLauncherFn, StageBuildFn},
        subagent::{SubagentChainAssembler, SubagentChainContext},
    },
    tools::DirectToolInvocationResolver,
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

    fn last_push_done_request_id(&self) -> Option<String> {
        self.push_done_request_ids
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
    }

    fn last_push_done_stop_reason(&self) -> Option<String> {
        self.push_done_stop_reasons.lock().unwrap().last().cloned()
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

/// 空操作 broker：短路路径不会触发任何交互，仅满足 SessionContext 构造。
struct NoopBroker;

#[async_trait]
impl UserInteractionBroker for NoopBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        InteractionResponse::Rejected
    }
}

// ── Mock 端口（短路路径不调用，仅满足 SessionContext 构造）───────────────

struct NoopEventPublisher;

impl EventPublisher for NoopEventPublisher {
    fn publish_event(&self, _session_id: &str, _source: &UnstampedEvent, _event: ExecutorEvent) {}
}

struct NoopSubscriber;

#[async_trait]
impl EventSubscriber for NoopSubscriber {
    async fn recv(&mut self) -> Result<EventMessage, SubscriptionError> {
        unreachable!("short-circuit path never subscribes")
    }

    fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        unreachable!("short-circuit path never subscribes")
    }
}

struct NoopSkills;

impl SkillsPort for NoopSkills {
    fn available_skills(&self, _cwd: &str, _plugin_roots: &[SkillRoot]) -> Vec<SkillMetadata> {
        Vec::new()
    }

    fn agents(
        &self,
        _cwd: &str,
        _extra_dirs: &[PathBuf],
    ) -> Vec<(String, String, String, AgentCapability)> {
        Vec::new()
    }
}

struct NoopToolSearch;

impl ToolSearchPort for NoopToolSearch {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// 空链装配器（短路路径不调用；assemble 返回空链即可满足类型）。
struct EmptyChainAssembler;

impl SubagentChainAssembler for EmptyChainAssembler {
    fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
        MiddlewareChain::new()
    }
}

/// 占位 stage 装配桥（短路路径不调用；满足 TurnInput 类型——被调用即测试失败）。
fn noop_stage_build() -> StageBuildFn {
    Arc::new(|_sbr| unreachable!("short-circuit path never builds stage"))
}

/// 占位 forwarder 启动器（短路路径不调用；满足 TurnInput 类型）。
fn noop_forwarder() -> ForwarderLauncherFn {
    Arc::new(|_handles, _agent_id, _on_event| {})
}

// ── Helper 工厂函数 ─────────────────────────────────────────────────────────

/// 构造最小 SessionContext（keepgoing 短路路径只用到 session_id，其余字段给默认值）。
fn make_session_context(session_id: &str) -> SessionContext {
    SessionContext {
        cwd: "/tmp".to_string(),
        provider_name: "test-provider".to_string(),
        provider_model_name: "test-model".to_string(),
        provider_fp: "test:model".to_string(),
        effective_context_window: 200_000,
        claude_md_excludes: None,
        language: None,
        compact_config: Default::default(),
        bg_llm_factory: Arc::new(|| Err("test context: bg llm factory not reachable".to_string())),
        get_cached_llm: None,
        fresh_auxiliary_model: None,
        store_llm: None,
        retry_events: None,
        primary_llm_factory: None,
        auto_classifier_factory: None,
        subagent_llm_factory: None,
        session_id: session_id.to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(NoopBroker),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        session_access: None,
        thread_store: None,
        thread_id: None,
        plugin_skill_roots: vec![],
        plugin_agent_dirs: vec![],
        plugin_loaded: vec![],
        hook_groups: vec![],
        cron_scheduler: None,
        mcp_pool: None,
        channel_state: None,
        tool_search_index: Arc::new(NoopToolSearch),
        shared_tools: Arc::new(parking_lot::RwLock::new(Default::default())),
        lsp_servers: vec![],
        lsp_pool: None,
        workflow_executor: None,
        skills: Arc::new(NoopSkills),
        workflow_middleware: None,
        event_publisher: Arc::new(NoopEventPublisher),
        subscribe: Arc::new(|| Box::new(NoopSubscriber)),
        command_lookup: Arc::new(|_| None),
        compact_config_loader: Arc::new(Default::default),
        parent_tools_factory: Arc::new(|| Arc::new(Vec::new())),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tool_invocation_resolver: Arc::new(DirectToolInvocationResolver),
        session_start_source: None,
        request_id: None,
        allow_await_wake: false,
        continuation_notify: None,
        frozen_fallback_builder: None,
    }
}

/// 构造基础 TurnInput（短路路径用；调用方可覆盖字段）。
fn make_turn_input(
    event_sink: Arc<dyn EventSink>,
    content: MessageContent,
    continuation: bool,
    history: Vec<BaseMessage>,
) -> TurnInput {
    TurnInput {
        event_sink,
        content,
        continuation,
        frozen: None,
        history,
        incoming_recalls: vec![],
        bg_results: vec![],
        langfuse: None,
        stage_build: noop_stage_build(),
        forwarder_launcher: noop_forwarder(),
    }
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
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        false,
        vec![],
    );
    let turn = TurnInput {
        // keepgoing 语义：不注入 recall（否则 recall 拼进 user 消息使其非空）
        incoming_recalls: vec!["should-be-skipped".to_string()],
        ..turn
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
    assert_eq!(
        mock_sink.last_push_done_stop_reason().as_deref(),
        Some("end_turn"),
        "keepgoing 短路的协议出口终态必须唯一且为 end_turn"
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
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        false,
        vec![],
    );

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
        "检测本身不应更新 last-notified（记账发生在入队时）"
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
