//! 生产链序契约测试（ARC-MIDDLEWARE-001 + 2026-07-25 技术债 issue）。
//!
//! 锁定「蓝本（`production_blueprint`）↔ 装配实现（`ProductionChainAssembler`）」
//! 的一一对应：完整序列精确断言 + 条件注册（Hook/MCP/Workflow/Goal）
//! 组合矩阵 + 权限模式不变性。任意中间件被重排、遗漏、重复注册或插入
//! 错误位置时，至少一条测试失败。
//!
//! 链序是行为契约（迁移自 `peri-acp/src/agent/builder.rs`），禁止按名称、
//! 便利性或局部需求重排——修改本文件的期望序列必须先同步修改蓝本。

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::{stream, TryStreamExt};
use parking_lot::RwLock;
use peri_agent::agent::workflow::WorkflowAgentContext;
use peri_agent::session::subagent::SubagentLlmSource;
use peri_agent::{
    agent::{
        async_tasks::TaskManager,
        events::{AgentEventHandler, ExecutorEvent},
        react::ToolCall,
        state::AgentState,
        AgentCancellationToken,
    },
    goal::{GoalController, GoalViewSnapshot},
    interaction::{
        InteractionContext, InteractionResponse, TrustedInteractionOwner, UserInteractionBroker,
    },
    middleware::chain::MiddlewareChain,
    session::factory::{build_middleware_chain, production_blueprint, ChainSlot},
    tools::BaseTool,
};
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason,
};
use peri_resources::workflow::protocol::{AgentRunParams, AgentRunResult};
use peri_resources::workflow::runner::AgentExecutor;

use crate::{
    assembly::{
        default_workflow_middleware_factory, default_workflow_middleware_factory_with_pool,
        AssemblyContext, OnBgCompleteFn, ProductionChainAssembler, SystemPromptBuilder,
    },
    hooks::{HookEvent, HookType, RegisteredHook},
    mcp::{McpClientHandle, McpClientPool},
    permission::{PermissionMode, SharedPermissionMode},
    tool_search::ToolSearchIndex,
    tools::TodoItem,
};
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::workspace::SessionBinding;
use peri_resources::sessions::SessionResourcesImpl;

// ── fakes ─────────────────────────────────────────────────────────────────────

struct FakeBroker;

#[async_trait]
impl UserInteractionBroker for FakeBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        InteractionResponse::Rejected
    }
}

struct FakeEventHandler;

impl AgentEventHandler for FakeEventHandler {
    fn on_event(&self, _event: ExecutorEvent) {}
}

#[derive(Clone)]
struct FakeLlm;

#[async_trait]
impl peri_model::Model for FakeLlm {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities::default()
    }

    fn prepare_stream(
        &self,
        _request: peri_model::ModelRequest,
    ) -> peri_model::ModelResult<peri_model::PreparedModelCall> {
        unimplemented!("契约测试不调用 LLM")
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        unimplemented!("契约测试不调用 LLM")
    }
}

struct FakeModel;

#[async_trait]
impl Model for FakeModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        unimplemented!("契约测试不调用模型")
    }
}

struct CancelGateModel {
    entered: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl Clone for CancelGateModel {
    fn clone(&self) -> Self {
        Self {
            entered: self.entered.clone(),
        }
    }
}

#[derive(Clone)]
struct CompletedWorkflowModel;

fn prepared_workflow_model_call<M: Model + Clone + 'static>(
    model: M,
    request: ModelRequest,
) -> ModelResult<peri_model::PreparedModelCall> {
    let checkpoint = serde_json::json!({
        "provider": "workflow-test",
        "model": "workflow-fixture",
        "endpoint": "https://workflow.test.invalid/stream",
        "credentialRef": "fixture:no-credential",
        "body": serde_json::to_value(&request).unwrap(),
    });
    Ok(peri_model::PreparedModelCall::new(
        checkpoint,
        move |cancellation| {
            let stream_cancellation = cancellation.clone();
            let events =
                stream::once(async move { model.stream(request, stream_cancellation).await })
                    .try_flatten();
            Ok(ModelStream::with_parent_cancellation(events, cancellation))
        },
    ))
}

#[async_trait]
impl Model for CompletedWorkflowModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<peri_model::PreparedModelCall> {
        prepared_workflow_model_call(self.clone(), request)
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        let response = ModelResponse::new(
            ModelMessage::assistant_text("workflow done"),
            StopReason::EndTurn,
            None,
            Some("workflow-forwarder-failure".into()),
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

#[async_trait]
impl Model for CancelGateModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<peri_model::PreparedModelCall> {
        prepared_workflow_model_call(self.clone(), request)
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        Ok(ModelStream::with_parent_cancellation(
            stream::pending::<ModelResult<ModelStreamEvent>>(),
            cancellation,
        ))
    }
}

struct FakeGoalController;

#[async_trait]
impl GoalController for FakeGoalController {
    async fn create_goal(&self, _objective: String) -> Result<(), String> {
        Ok(())
    }
    async fn complete_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn block_goal(&self, _reason: String) -> Result<(), String> {
        Ok(())
    }
    async fn clear_goal(&self) -> Result<(), String> {
        Ok(())
    }
    fn snapshot(&self) -> GoalViewSnapshot {
        unimplemented!("契约测试不调用 goal snapshot")
    }
}

// ── 装配上下文构造 ────────────────────────────────────────────────────────────

/// 最小装配上下文（全部条件关闭，权限模式 Default）。
fn base_context() -> AssemblyContext {
    let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);
    let (bg_event_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    let llm_factory: Arc<dyn Fn(Option<&str>) -> SubagentLlmSource + Send + Sync> =
        Arc::new(|_model_alias| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(FakeLlm),
                "fixture-scripted",
            )
        });
    let system_builder: SystemPromptBuilder =
        Arc::new(|_overrides: Option<&AgentOverrides>, _cwd: &str| String::new());
    let on_bg_complete: Option<OnBgCompleteFn> = None;

    AssemblyContext {
        // 生产装配面同款端口（未绑 registry ⇒ 空目录）；池存在时 fail-closed
        // 校验要求它是具体实现。
        agent_catalog: Arc::new(crate::host_ports::AgentCatalogProvider::new()),
        cwd: "/tmp/contract-test".to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(FakeBroker),
        permission_mode: SharedPermissionMode::new(PermissionMode::Default),
        model_name: "contract-model".to_string(),
        provider_name: "contract-provider".to_string(),
        auxiliary_model: None,
        auto_classifier_model: Arc::new(tokio::sync::Mutex::new(
            Box::new(FakeModel) as Box<dyn Model>
        )),
        preload_skills: Vec::new(),
        plugin_skill_roots: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        session_start_source: None,
        mcp_skill_registry: None,
        command_registry: None,
        cron_scheduler: None,
        mcp_pool: None,
        session_id: "session-contract-test".to_string(),
        tool_search_index: Arc::new(ToolSearchIndex::new()),
        shared_tools,
        event_handler: Arc::new(FakeEventHandler),
        task_manager: Arc::new(TaskManager::new()),
        bg_event_tx,
        on_bg_complete,
        langfuse_bridge: None,
        session_resources: None,
        parent_thread_id: None,
        register_runtime: None,
        deregister_runtime: None,
        child_handler_factory: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        system_prompt_for_sub: String::new(),
        llm_factory,
        system_builder,
        todo_tx,
        goal_controller: None,
        meta_harness_disabled: std::collections::HashSet::new(),
        // 契约夹具不消费 beta flag 语义：缺省（前台）与 flag 引入前一致。
        agent_default_run_in_background: false,
        agent_overrides: None,
        language: None,
    }
}

/// 装配并返回链上中间件名称序列。
fn assemble_names(ctx: &AssemblyContext) -> Vec<String> {
    let out = build_middleware_chain(&ProductionChainAssembler, ctx).expect("assembly");
    out.chain.names().into_iter().map(String::from).collect()
}

#[test]
fn skills_face_closed_suppresses_automatic_preload_but_keeps_explicit_list() {
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SkillsMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|name| name == "SkillPreloadMiddleware"));

    ctx.preload_skills.push("declared-skill".to_string());
    let names = assemble_names(&ctx);
    assert!(names.iter().any(|name| name == "SkillPreloadMiddleware"));
}

fn make_hook() -> RegisteredHook {
    RegisteredHook {
        hook: HookType::Command {
            command: "echo hi".to_string(),
            shell: None,
            timeout: None,
            status_message: None,
            once: false,
            async_run: false,
            async_rewake: false,
            matcher: None,
            condition: None,
        },
        event: HookEvent::PreToolUse,
        matcher: None,
        plugin_name: "test-plugin".to_string(),
        plugin_id: "test-plugin-id".to_string(),
        plugin_source: None,
        plugin_root: PathBuf::from("/tmp/test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
        plugin_options: Default::default(),
    }
}

/// 生产装配（而非直接构造 HookMiddleware）必须把会话 ID 注入 command hook 的 stdin。
#[cfg(unix)]
#[tokio::test]
async fn production_assembly_passes_session_id_to_command_hook_stdin() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("stdin.json");
    let mut hook = make_hook();
    if let HookType::Command { command, .. } = &mut hook.hook {
        *command = r#"cat > "$CLAUDE_PLUGIN_OPTION_OUT""#.to_string();
    }
    hook.plugin_options.insert(
        "out".to_string(),
        serde_json::json!(output.to_str().unwrap()),
    );

    let mut ctx = base_context();
    ctx.cwd = directory.path().to_string_lossy().into_owned();
    ctx.hook_groups = vec![vec![hook]];

    let mut chain = MiddlewareChain::new();
    super::hooks::add_hooks(&ctx, &mut chain);
    chain
        .run_before_tool(
            &mut AgentState::new(ctx.cwd.clone()),
            ToolCall::new("call", "Bash", serde_json::json!({"command":"test"})),
        )
        .await
        .expect("session_id 注入不应改变 hook 的放行结果");

    let written = std::fs::read_to_string(&output).expect("hook 应把 stdin 写入文件");
    let input: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(input["session_id"], "session-contract-test");
}

/// deployment pool：两个 builtin 实例（`web` / `artifact`）的假「已连接」handle，
/// 工具清单与注册表一致。用于断言 builtin 能力的三个工具面（A6）。
fn pool_with_builtin_instances() -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        let tools: Vec<rmcp::model::Tool> = instance
            .tools
            .iter()
            .map(|tool| {
                serde_json::from_value(serde_json::json!({
                    "name": tool.original_name,
                    "description": "builtin tool",
                    "inputSchema": { "type": "object", "properties": {} }
                }))
                .unwrap()
            })
            .collect();
        pool.clients.write().insert(
            instance.name.to_string(),
            make_connected_handle(instance.name, tools),
        );
    }
    pool
}

fn make_connected_handle(server: &str, tools: Vec<rmcp::model::Tool>) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: server.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: None,
        tools,
        resources: vec![],
        status: crate::mcp::ClientStatus::Connected,
        oauth_status: Default::default(),
        source: Some(peri_acp_types::plugin::ConfigSource::Builtin {
            instance: server.to_string(),
        }),
        url: None,
        skills_capable: false,
    })
}

/// 装配并返回链上中间件 collect_tools 的工具名集合。
fn assemble_tool_names(ctx: &AssemblyContext) -> Vec<String> {
    let out = build_middleware_chain(&ProductionChainAssembler, ctx).expect("assembly");
    out.chain
        .collect_tools(&ctx.cwd)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect()
}

/// workflow 上下文（可注入 broker / permission_mode 的矩阵变体，D3 对拍用）。
fn workflow_context_with_approval(
    disabled: &[&str],
    broker_present: bool,
    mode_present: bool,
) -> WorkflowAgentContext {
    let mut ctx = workflow_context_with_disabled(disabled);
    if broker_present {
        ctx.broker = Some(Arc::new(FakeBroker));
    }
    if mode_present {
        ctx.permission_mode = Some(SharedPermissionMode::new(PermissionMode::Default));
    }
    ctx
}

fn workflow_context_with_disabled(disabled: &[&str]) -> WorkflowAgentContext {
    let model_factory: peri_agent::agent::workflow::factory::WorkflowModelFactory =
        Arc::new(|_model, _max_tokens, _observer| unimplemented!("契约测试不调用"));
    let prompt_builder: peri_agent::agent::workflow::factory::WorkflowAgentPromptBuilder =
        Arc::new(|_, _, _, _| String::new());
    let fallback: peri_agent::agent::workflow::factory::WorkflowSystemPromptFallback =
        Arc::new(|_, _, _| String::new());
    let forwarder: peri_agent::session::exec::executor_helpers::ForwarderLauncherFn =
        Arc::new(|_, _, _| tokio::spawn(async {}));
    WorkflowAgentContext {
        cwd: "/tmp/contract-test".to_string(),
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        mcp_skill_registry: None,
        session_id: None,
        session_resources: None,
        compact_config: None,
        cancel: None,
        system_prompt: None,
        external_instructions: None,
        environment_prompt: None,
        legacy_embedded_instructions: false,
        broker: None,
        permission_mode: None,
        frozen_date: None,
        frozen_language: None,
        progress_tx: None,
        subagent_ctx_builder: None,
        agent_prompt_builder: prompt_builder,
        model_factory,
        middleware_factory: default_workflow_middleware_factory(),
        system_prompt_fallback: fallback,
        forwarder_launcher: forwarder,
        publish_hook: None,
        langfuse_hooks: None,
        langfuse_event_handler: None,
        meta_harness_disabled: disabled.iter().map(|s| s.to_string()).collect(),
    }
}

struct WorkflowExecutionFixture {
    _directory: tempfile::TempDir,
    resources: Arc<dyn SessionResources>,
    cwd: String,
    parent_id: String,
}

impl WorkflowExecutionFixture {
    async fn new(parent_id: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(directory.path().join("threads.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        resources
            .create_session(&NewSession {
                thread_id: parent_id.to_owned(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: cwd.clone(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
            })
            .await
            .unwrap();
        Self {
            _directory: directory,
            resources,
            cwd,
            parent_id: parent_id.to_owned(),
        }
    }

    fn bind(&self, context: &mut WorkflowAgentContext) {
        context.cwd = self.cwd.clone();
        context.session_id = Some(self.parent_id.clone());
        context.session_resources = Some(self.resources.clone());
    }
}

/// 槽位 → middleware `name()` 返回值映射（与 assembly.rs 装配分支一一对应）。
fn slot_middleware_name(slot: &ChainSlot) -> &'static str {
    match slot {
        ChainSlot::DefaultSystemPrompt => "DefaultSystemPromptMiddleware",
        ChainSlot::Lang => "LangMiddleware",
        ChainSlot::AgentsMd => "AgentsMdMiddleware",
        ChainSlot::Plugin => "PluginMiddleware",
        ChainSlot::Skills => "SkillsMiddleware",
        ChainSlot::SkillPreload => "SkillPreloadMiddleware",
        ChainSlot::AtMention => "AtMentionMiddleware",
        ChainSlot::Image => "ImageMiddleware",
        ChainSlot::GitAttribution => "GitAttributionMiddleware",
        ChainSlot::Todo => "TodoMiddleware",
        ChainSlot::Hook => "HookMiddleware",
        ChainSlot::Permission => "PermissionMiddleware",
        ChainSlot::AskUser => "HumanInTheLoopMiddleware",
        ChainSlot::SubAgent => "SubAgentMiddleware",
        ChainSlot::Mcp => "McpMiddleware",
        ChainSlot::ToolSearch => "ToolSearch",
        ChainSlot::Goal => "GoalMiddleware",
    }
}

#[path = "assembly_test_baseline.rs"]
mod baseline;
#[path = "assembly_test_meta.rs"]
mod meta;
#[path = "assembly_once_scope_test.rs"]
mod once_scope;
#[path = "assembly_test_sections.rs"]
mod sections;
#[path = "assembly_test_workflow.rs"]
mod workflow;

// ─── beta flag `full-async-tools`（装配面 → Agent 工具 schema）──────────────

/// 装配参数 → 链装配产物：`AssemblyContext::agent_default_run_in_background`
/// 必须到达链上 `Agent` 工具的 schema（`run_in_background.default`），
/// 且缺省注入与否只改这一项。
///
/// 接线断掉（例如 `ProductionChainAssembler` 不再把它交给 `SubAgentMiddleware`、
/// 或 `build_tool` 不再下传）时本用例报红——工具级用例直接调
/// `with_default_run_in_background` 绕过装配，覆盖不到这段。
#[test]
fn agent_default_run_in_background_reaches_chain_tool_schema() {
    fn agent_tool_default(ctx: &AssemblyContext) -> serde_json::Value {
        let out = build_middleware_chain(&ProductionChainAssembler, ctx).expect("assembly");
        let tools = out.chain.collect_tools(&ctx.cwd);
        let agent = tools
            .iter()
            .find(|tool| tool.name() == "Agent")
            .expect("生产链必须提供 Agent 工具");
        agent.parameters()["properties"]["run_in_background"]["default"].clone()
    }

    let mut ctx = base_context();
    assert_eq!(agent_tool_default(&ctx), serde_json::json!(false));

    ctx.agent_default_run_in_background = true;
    assert_eq!(agent_tool_default(&ctx), serde_json::json!(true));
}

// ─── 端口不匹配：fail-closed（不造空池/默认索引） ─────────────────────────────

/// 非本 crate 的池实现：`as_any` 合法返回 self，但具体类型不是 `McpClientPool`。
struct ForeignPool;

#[async_trait]
impl peri_acp_types::ports::McpPoolPort for ForeignPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) -> peri_acp_types::ports::McpPoolShutdownReport {
        peri_acp_types::ports::McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}

struct ForeignToolSearch;

impl peri_acp_types::ports::ToolSearchPort for ForeignToolSearch {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn assert_port_mismatch(ctx: &AssemblyContext, port: &'static str, expected: &'static str) {
    let error = match build_middleware_chain(&ProductionChainAssembler, ctx) {
        Ok(_) => panic!("端口存在但类型不符必须显式失败，不能静默替换为降级实例"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        peri_agent::session::factory::AssemblyError::PortMismatch { port, expected },
        "错误必须指出不匹配的端口与期望实现"
    );
}

#[test]
fn mcp_pool_port_mismatch_fails_assembly_instead_of_empty_pool() {
    let mut ctx = base_context();
    ctx.mcp_pool = Some(Arc::new(ForeignPool));
    assert_port_mismatch(&ctx, "McpPoolPort", "McpClientPool");
}

#[test]
fn tool_search_port_mismatch_fails_assembly_instead_of_default_index() {
    let mut ctx = base_context();
    ctx.tool_search_index = Arc::new(ForeignToolSearch);
    assert_port_mismatch(&ctx, "ToolSearchPort", "ToolSearchIndex");
}

/// 池是真实实现、目录端口是外来实现：目录会静默为空，必须失败而不是继续装配。
#[test]
fn agent_catalog_port_mismatch_fails_assembly_when_pool_is_present() {
    let mut ctx = base_context();
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_pending()));
    // 测试用外来目录端口：有池时目录必须可绑定，不能静默为空。
    ctx.agent_catalog = Arc::new(crate::host_ports::NoopAgentCatalog);
    assert_port_mismatch(&ctx, "AgentCatalogPort", "AgentCatalogProvider");
}

/// 无 MCP 是合法装配路径（print / 未声明 MCP）：不得误报端口不匹配。
#[test]
fn no_mcp_pool_is_a_legal_assembly_path() {
    let ctx = base_context();
    assert!(ctx.mcp_pool.is_none());
    build_middleware_chain(&ProductionChainAssembler, &ctx).expect("无 MCP 必须可装配");
}

/// 生产注入的具体实例必须被原样还原（装配产物与注入共享同一 `Arc`）。
#[test]
fn concrete_ports_bind_the_injected_instances() {
    let mut ctx = base_context();
    let pool = Arc::new(McpClientPool::new_pending());
    let index = Arc::new(ToolSearchIndex::new());
    ctx.mcp_pool = Some(Arc::clone(&pool) as Arc<dyn peri_acp_types::ports::McpPoolPort>);
    ctx.tool_search_index = Arc::clone(&index) as Arc<dyn peri_acp_types::ports::ToolSearchPort>;

    let resolved = crate::assembly::preparation::resolve_ports(&ctx).expect("生产端口必须可还原");
    assert!(
        Arc::ptr_eq(resolved.mcp_pool_concrete.as_ref().unwrap(), &pool),
        "注入池与装配产物必须是同一实例"
    );
    assert!(
        Arc::ptr_eq(&resolved.tool_search_index_concrete, &index),
        "注入索引与装配产物必须是同一实例"
    );
    assert!(
        resolved.mcp_agent_registry.is_some(),
        "有池时必须构造 registry"
    );
}
