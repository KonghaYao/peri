//! executor.rs 单元测试（L5：自 `peri-acp/src/host/exec/executor_test.rs` 随迁）。
//!
//! 归属说明：keepgoing 判定三例 + keepgoing 短路 push_done（TRAP）+
//! request_id 透传随 `run_session_loop` 迁入本 crate（ARC-KEEPGOING-001
//! 契约测试）；完整装配路径测试（continuation / turn 终态唯一 / frozen
//! 渲染）留在 ACP 宿主侧（`host/executor_flow_test.rs`——stage 装配注入面
//! 在 ACP，测试经宿主构造点注入真实 stage_build 桥）。
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

use super::{is_keepgoing, run_session_loop, PromptStopReason, SessionContext, TurnInput};
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
        meta_harness: Default::default(),
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
