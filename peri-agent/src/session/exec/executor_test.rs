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
        // 测试泵：永不产生事件。pump 的 biased select 会先 poll 本分支，
        // 必须返回 pending（而非 unreachable）——agent 管线路径（Inject/
        // PassThrough）会 spawn pump，select 另一分支（channel 关闭）就绪
        // 时自然退出。
        std::future::pending().await
    }

    fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        Ok(None)
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

// ── run_session_loop: 命令拦截三态分发（Phase 5 Step 6）─────────────────────
//
// 三态：Handled → 短路返回（agent 不构建，stage_build 哨兵不被调用）；
// Inject / PassThrough → 走 agent 管线（管线分发核心断言在
// executor_helpers_test 的 intercept_immediate_command 层，本层测可端到端
// 验证的短路分支；noop_stage_build 被调用即 panic——见 noop_stage_build 注释）。

/// 构造「命中 Done handler」的 command_lookup（/compact 形态，恒 Done）。
fn done_command_lookup() -> super::CommandLookupFn {
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
    };
    use peri_acp_types::command::{CommandHandler, CommandOutcome, CommandResult, ResolvedCommand};
    struct DoneHandler;
    #[async_trait]
    impl CommandHandler for DoneHandler {
        async fn execute(&self, ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
            CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: None,
            })
        }
    }
    Arc::new(move |text: &str| {
        if text == "compact" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:compact".to_string(),
                    aliases: vec![],
                    description: "compact for executor dispatch test".to_string(),
                    kind: CommandEntryKind::Command,
                    category: None,
                    args_schema: None,
                    handler: Arc::new(DoneHandler),
                    provenance: CommandProvenance {
                        source: CommandSource::Core,
                        lifecycle: CommandLifecycle::Connected,
                    },
                }),
                args: String::new(),
            })
        } else {
            None
        }
    })
}

/// Handled 分发：拦截命中 → run_session_loop 短路返回（ok / EndTurn），
/// push_done 恰好一次，且不构建 agent（stage_build 哨兵不被调用——noop
/// stage_build 被调用即 panic）。
#[tokio::test]
async fn test_run_session_loop_intercept_handled_short_circuits() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let mut ctx = make_session_context("test-session");
    ctx.command_lookup = done_command_lookup();
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text("/compact"),
        false,
        vec![BaseMessage::human("hello")],
    );

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：短路返回，agent 不构建
    assert!(result.ok, "Handled 分发应 ok=true");
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "Handled 分发必须 push_done 一次（TRAP 守护）"
    );
    assert!(
        result.recall_items.is_empty(),
        "Handled 分发不应产生 recall items"
    );
}

/// cancel 分发：外层 cancel 已触发 + handler pending → 拦截层
/// Handled(Cancelled)，run_session_loop 短路返回 Cancelled + push_done。
#[tokio::test]
async fn test_run_session_loop_intercept_cancel_returns_cancelled() {
    // Arrange
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
    };
    use peri_acp_types::command::{CommandHandler, CommandOutcome, ResolvedCommand};
    struct PendingHandler;
    #[async_trait]
    impl CommandHandler for PendingHandler {
        async fn execute(&self, _ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
            std::future::pending::<()>().await;
            unreachable!("pending 永不返回");
        }
    }
    let lookup: super::CommandLookupFn = Arc::new(|text: &str| {
        if text == "compact" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:compact".to_string(),
                    aliases: vec![],
                    description: "compact for executor cancel test".to_string(),
                    kind: CommandEntryKind::Command,
                    category: None,
                    args_schema: None,
                    handler: Arc::new(PendingHandler),
                    provenance: CommandProvenance {
                        source: CommandSource::Core,
                        lifecycle: CommandLifecycle::Connected,
                    },
                }),
                args: String::new(),
            })
        } else {
            None
        }
    });

    let mock_sink = Arc::new(MockEventSink::new());
    let mut ctx = make_session_context("test-session");
    ctx.command_lookup = lookup;
    ctx.cancel.cancel();
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text("/compact"),
        false,
        vec![BaseMessage::human("hello")],
    );

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：cancel 分支 → Cancelled + history 原样 + push_done
    assert!(result.ok);
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(result.messages.len(), 1, "cancel 应返回原样 history");
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "cancel 分发必须 push_done 一次（TRAP 守护）"
    );
}

/// Inject 分发（Phase 5 Step 6）：拦截命中返回 `Inject` 的 handler →
/// run_session_loop **不短路**——注入文本经 `AgentInput::blocks` 进入 agent
/// 管线（stage_build 被调用、v2 queue 收到注入文本），pump 正常收尾
/// （push_done 恰好一次）。与 Handled 分发（短路、不构建 agent）成对。
///
/// 管线端到端：stage_build 哨兵返回「turn 已取消」的最小 V2AgentOutput——
/// run_react_loop 首轮检查 `turn.is_cancelled()` 立即 `Interrupted`，管线
/// 以 Cancelled 收尾，无需真实 LLM。
#[tokio::test]
async fn test_run_session_loop_intercept_inject_enters_agent_pipeline() {
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
    };
    use peri_acp_types::command::{CommandHandler, CommandOutcome, ResolvedCommand};
    use peri_acp_types::event_v2::EventBus;
    use peri_acp_types::session::{MessageKind, MessageSource as V2MessageSource};

    use crate::agent::stages::StageContext;
    use crate::session::exec::stage_builder::V2AgentOutput;
    use crate::session::{FrozenContext, Session};

    // Arrange：命中返回 Inject 的 handler（/inject 形态，恒 Inject）。
    struct InjectHandler;
    #[async_trait]
    impl CommandHandler for InjectHandler {
        async fn execute(&self, _ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
            CommandOutcome::Inject("/skill tdd".to_string())
        }
    }
    let lookup: super::CommandLookupFn = Arc::new(|text: &str| {
        if text == "inject" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:inject".to_string(),
                    aliases: vec![],
                    description: "inject for executor dispatch test".to_string(),
                    kind: CommandEntryKind::Command,
                    category: None,
                    args_schema: None,
                    handler: Arc::new(InjectHandler),
                    provenance: CommandProvenance {
                        source: CommandSource::Core,
                        lifecycle: CommandLifecycle::Connected,
                    },
                }),
                args: String::new(),
            })
        } else {
            None
        }
    });

    // stage_build 哨兵：记录调用次数 + 返回「turn 已取消」的最小
    // V2AgentOutput（run_react_loop 立即 Interrupted，无需真实 LLM）。
    let stage_build_calls = Arc::new(Mutex::new(0usize));
    let build_calls = Arc::clone(&stage_build_calls);
    let session_for_build = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let session_arc = Arc::clone(&session_for_build);
    let stage_build: StageBuildFn = Arc::new(move |_sbr| {
        *build_calls.lock().unwrap() += 1;
        let turn = session_for_build.start_turn();
        turn.cancel_token.cancel(); // 首轮 is_cancelled() → 立即 Interrupted
        let (bus, handles) = EventBus::new(Default::default());
        let ctx = StageContext::builder(
            turn,
            session_for_build.transcript(),
            session_for_build.queue().clone(),
        )
        .with_event_bus(Arc::new(bus))
        .build();
        let (_todo_tx, todo_rx) = tokio::sync::mpsc::channel(8);
        let (_bg_tx, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            V2AgentOutput {
                context: ctx,
                session: Arc::clone(&session_for_build),
                event_handles: handles,
                todo_rx,
                bg_event_rx,
            },
            None,
        )
    });

    let mock_sink = Arc::new(MockEventSink::new());
    let mut ctx = make_session_context("test-session");
    ctx.command_lookup = lookup;
    let mut turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text("/inject"),
        false,
        vec![BaseMessage::human("hello")],
    );
    turn.stage_build = stage_build;

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：Inject 不短路——agent 管线被构建，注入文本进 v2 queue。
    assert_eq!(
        *stage_build_calls.lock().unwrap(),
        1,
        "Inject 分发必须构建 agent 管线（stage_build 被调用一次）"
    );
    let queued = session_arc.queue().drain_all();
    let injected = queued.iter().find(|q| q.kind == MessageKind::Prompt);
    assert!(
        matches!(injected, Some(q) if q.message.content().contains("/skill tdd")),
        "注入文本必须经 AgentInput::blocks 进入 agent 管线，实际: {:?}",
        injected.map(|q| q.message.content())
    );
    assert!(
        matches!(injected, Some(q) if q.source == V2MessageSource::UserInput),
        "注入消息来源应为 UserInput"
    );
    // 管线以 Interrupted(Cancelled) 收尾（哨兵 turn 已取消）；pump 正常收尾
    // 恰好一次 push_done（agent pump 负责，拦截层不 push）。
    assert!(!result.ok, "Interrupted 应 ok=false");
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "Inject 分发不 push_done 于拦截层，pump 收尾恰好一次（TRAP 守护）"
    );
}
