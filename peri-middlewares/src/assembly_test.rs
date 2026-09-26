//! 生产链序契约测试（ARC-MIDDLEWARE-001 + 2026-07-25 技术债 issue）。
//!
//! 锁定「蓝本（`production_blueprint`）↔ 装配实现（`ProductionChainAssembler`）」
//! 的一一对应：完整序列精确断言 + 条件注册（Hook/MCP/Workflow/LSP/Goal）
//! 组合矩阵 + 权限模式不变性。任意中间件被重排、遗漏、重复注册或插入
//! 错误位置时，至少一条测试失败。
//!
//! 链序是行为契约（迁移自 `peri-acp/src/agent/builder.rs`），禁止按名称、
//! 便利性或局部需求重排——修改本文件的期望序列必须先同步修改蓝本。

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::RwLock;
use peri_agent::{
    agent::{
        async_tasks::TaskManager,
        events::{AgentEventHandler, ExecutorEvent},
        react::ReactLLM,
        AgentCancellationToken,
    },
    goal::{GoalController, GoalViewSnapshot},
    interaction::{InteractionContext, InteractionResponse, UserInteractionBroker},
    session::factory::{build_middleware_chain, production_blueprint, ChainSlot},
    tools::BaseTool,
};
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason,
};
use peri_resources::lsp::config::{LspConfigSource, LspServerConfig};
use peri_resources::workflow::protocol::{AgentRunParams, AgentRunResult};
use peri_resources::workflow::runner::AgentExecutor;

use crate::{
    agent_define::AgentOverrides,
    assembly::{
        create_session_lsp_pool, default_workflow_middleware_factory,
        default_workflow_middleware_factory_with_pool, load_merged_lsp_servers, AssemblyContext,
        OnBgCompleteFn, ProductionChainAssembler, SystemPromptBuilder,
    },
    hooks::{HookEvent, HookType, RegisteredHook},
    mcp::{McpClientHandle, McpClientPool},
    permission::{PermissionMode, SharedPermissionMode},
    tool_search::ToolSearchIndex,
    tools::TodoItem,
};

// ── fakes ─────────────────────────────────────────────────────────────────────

struct FakeBroker;

#[async_trait]
impl UserInteractionBroker for FakeBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        InteractionResponse::Rejected
    }
}

struct FakeDynamicDeployment;

#[async_trait]
impl peri_acp_types::ports::DynamicMcpDeploymentPort for FakeDynamicDeployment {
    async fn execute(
        &self,
        _session_id: &str,
        _action: peri_acp_types::dynamic_mcp::CanonicalDynamicMcpAction,
    ) -> Result<
        peri_acp_types::dynamic_mcp::DynamicMcpResponse,
        peri_acp_types::dynamic_mcp::DynamicMcpFailure,
    > {
        unimplemented!("assembly contract does not execute DynamicMCP")
    }

    fn register_catalog(
        &self,
        _session_id: &str,
        _tools: Vec<peri_acp_types::dynamic_mcp::DynamicMcpCatalogTool>,
    ) -> Result<(), peri_acp_types::dynamic_mcp::DynamicMcpFailure> {
        Ok(())
    }

    fn capability(
        &self,
        _session_id: &str,
    ) -> Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort> {
        struct Empty;
        impl peri_acp_types::ports::SessionMcpCapabilityPort for Empty {
            fn snapshot(&self) -> Arc<peri_acp_types::dynamic_mcp::SessionMcpCapabilitySnapshot> {
                Arc::new(Default::default())
            }

            fn bind_projection(
                &self,
                _static_handles: Vec<(String, peri_acp_types::mcp_skills::HandleToken)>,
                _skill_registry: Arc<peri_acp_types::mcp_skills::McpSkillRegistry>,
                _command_registry: Arc<peri_acp_types::command_registry::CommandRegistry>,
            ) -> Arc<dyn peri_acp_types::ports::SessionMcpProjectionLease> {
                struct Lease;
                impl peri_acp_types::ports::SessionMcpProjectionLease for Lease {
                    fn as_any(&self) -> &dyn std::any::Any {
                        self
                    }

                    fn refresh(&self) -> bool {
                        true
                    }

                    fn close(&self) {}
                }
                Arc::new(Lease)
            }
        }
        Arc::new(Empty)
    }

    fn close_registration(
        &self,
        _session_id: &str,
    ) -> Arc<dyn peri_acp_types::ports::SessionCloseRegistration> {
        struct Noop;
        #[async_trait]
        impl peri_acp_types::ports::SessionCloseRegistration for Noop {
            async fn revoke_and_cleanup(
                &self,
            ) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
                peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
            }
        }
        Arc::new(Noop)
    }

    fn begin_shutdown(&self) {}

    async fn close_session(
        &self,
        _session_id: &str,
    ) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
        peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
    }

    async fn shutdown(&self) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
        peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
    }
}

struct FakeEventHandler;

impl AgentEventHandler for FakeEventHandler {
    fn on_event(&self, _event: ExecutorEvent) {}
}

struct FakeLlm;

#[async_trait]
impl ReactLLM for FakeLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[peri_agent::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<peri_agent::agent::react::StreamingContext>,
    ) -> peri_agent::error::AgentResult<peri_agent::agent::react::Reasoning> {
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
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

struct CompletedWorkflowModel;

#[async_trait]
impl Model for CompletedWorkflowModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
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

struct FakeAgentExecutor;

#[async_trait]
impl AgentExecutor for FakeAgentExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        unimplemented!("契约测试不执行 workflow")
    }
}

// ── 装配上下文构造 ────────────────────────────────────────────────────────────

/// 最小装配上下文（全部条件关闭，权限模式 Default）。
fn base_context() -> AssemblyContext {
    let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);
    let (bg_event_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|_model_alias| Box::new(FakeLlm));
    let system_builder: SystemPromptBuilder =
        Arc::new(|_overrides: Option<&AgentOverrides>, _cwd: &str| String::new());
    let on_bg_complete: Option<OnBgCompleteFn> = None;

    AssemblyContext {
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
        claude_md_excludes: Vec::new(),
        preload_skills: Vec::new(),
        plugin_skill_roots: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        session_start_source: None,
        mcp_skill_registry: None,
        command_registry: None,
        cron_scheduler: None,
        mcp_pool: None,
        dynamic_mcp: None,
        dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
        session_id: "session-contract-test".to_string(),
        channel_state: None,
        tool_search_index: Arc::new(ToolSearchIndex::new()),
        shared_tools,
        lsp_servers: Vec::new(),
        lsp_pool: None,
        workflow_executor: None,
        workflow_middleware: None,
        event_handler: Arc::new(FakeEventHandler),
        task_manager: Arc::new(TaskManager::new()),
        bg_event_tx,
        on_bg_complete,
        langfuse_bridge: None,
        thread_store: None,
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
        agent_overrides: None,
        language: None,
    }
}

/// 装配并返回链上中间件名称序列。
fn assemble_names(ctx: &AssemblyContext) -> Vec<String> {
    let out = build_middleware_chain(&ProductionChainAssembler, ctx);
    out.chain.names().into_iter().map(String::from).collect()
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
        plugin_root: PathBuf::from("/tmp/test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
        plugin_options: Default::default(),
    }
}

fn make_lsp_config() -> LspServerConfig {
    LspServerConfig {
        name: "test-lsp".to_string(),
        command: "test-lsp-bin".to_string(),
        args: Vec::new(),
        env: None,
        extension_to_language: Default::default(),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    }
}

// ── 契约用例 ─────────────────────────────────────────────────────────────────

/// 蓝本槽位顺序 = 行为契约（7 组 25 槽，禁止重排；波 4 演进 C2 新增
/// DefaultSystemPrompt / Lang 于第一组首位——渲染排序不依赖链序，契约 2）。
///
/// v4-part-2 W3（A7/A14）：`Web` / `Artifact` 两槽位随 `WebMiddleware` /
/// `ArtifactMiddleware` 提供面一并删除（能力改由 builtin MCP 实例提供），
/// 其余槽位**相对顺序不变**——本断言即「过滤掉被删两项后与上一批次逐项相等」。
#[test]
fn blueprint_sequence_is_canonical() {
    let slots = production_blueprint();
    let names: Vec<&str> = slots.iter().map(|s| slot_name(s)).collect();
    assert_eq!(
        names,
        vec![
            // 第一组：上下文注入器
            "DefaultSystemPrompt",
            "Lang",
            "AgentsMd",
            "AgentDefine",
            "Plugin",
            "Skills",
            "SkillPreload",
            "AtMention",
            "Image",
            // 第二组：文件/终端工具提供器
            "Filesystem",
            "GitAttribution",
            "GitWatch",
            "Terminal",
            // 第三组：Todo / Cron
            "Todo",
            "Cron",
            // 第四组：Hook 哨兵
            "Hook",
            // 第五组：Permission + AskUser + SubAgent（2026-08-15 职责拆分）
            "Permission",
            "AskUser",
            "SubAgent",
            // 第六组：MCP / Workflow / PTC / ToolSearch
            "Mcp",
            "Workflow",
            "Ptc",
            "ToolSearch",
            // 第七组：LSP / Goal（Goal 在链最后）
            "Lsp",
            "Goal",
        ]
    );
}

fn slot_name(slot: &ChainSlot) -> &'static str {
    match slot {
        ChainSlot::DefaultSystemPrompt => "DefaultSystemPrompt",
        ChainSlot::Lang => "Lang",
        ChainSlot::AgentsMd => "AgentsMd",
        ChainSlot::AgentDefine => "AgentDefine",
        ChainSlot::Plugin => "Plugin",
        ChainSlot::Skills => "Skills",
        ChainSlot::SkillPreload => "SkillPreload",
        ChainSlot::AtMention => "AtMention",
        ChainSlot::Image => "Image",
        ChainSlot::Filesystem => "Filesystem",
        ChainSlot::GitAttribution => "GitAttribution",
        ChainSlot::GitWatch => "GitWatch",
        ChainSlot::Terminal => "Terminal",
        ChainSlot::Todo => "Todo",
        ChainSlot::Cron => "Cron",
        ChainSlot::Hook => "Hook",
        ChainSlot::Permission => "Permission",
        ChainSlot::AskUser => "AskUser",
        ChainSlot::SubAgent => "SubAgent",
        ChainSlot::Mcp => "Mcp",
        ChainSlot::Workflow => "Workflow",
        ChainSlot::Ptc => "Ptc",
        ChainSlot::ToolSearch => "ToolSearch",
        ChainSlot::Lsp => "Lsp",
        ChainSlot::Goal => "Goal",
    }
}

/// 默认配置（全条件关闭）下的完整链序列，与迁移前 builder 完全一致。
#[test]
fn default_config_produces_canonical_chain() {
    let ctx = base_context();
    assert_eq!(
        assemble_names(&ctx),
        vec![
            "DefaultSystemPromptMiddleware",
            "LangMiddleware",
            "AgentsMdMiddleware",
            "AgentDefineMiddleware",
            "PluginMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "AtMentionMiddleware",
            "ImageMiddleware",
            "FilesystemMiddleware",
            "GitAttributionMiddleware",
            "GitWatchMiddleware",
            "TerminalMiddleware",
            "TodoMiddleware",
            "CronMiddleware",
            "PermissionMiddleware",
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "PtcMiddleware",
            "ToolSearch",
        ]
    );
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
        name: server.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools,
        resources: vec![],
        status: crate::mcp::ClientStatus::Connected,
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
    })
}

/// builtin `artifact` 实例默认可用（`mcp__artifact__artifact` direct）；
/// 单独关闭 `ArtifactMiddleware` 后仅移除该实例的工具，不影响 ToolSearch 元工具。
#[test]
fn builtin_artifact_instance_can_be_closed_independently() {
    let mut enabled = base_context();
    enabled.mcp_pool = Some(pool_with_builtin_instances());
    let enabled_tools = assemble_tool_names(&enabled);
    for expected in [
        "mcp__artifact__artifact",
        "mcp__web__WebSearch",
        "SearchExtraTools",
        "ExecuteExtraTool",
    ] {
        assert!(
            enabled_tools.iter().any(|name| name == expected),
            "未关闭时 {expected} 应可见: {enabled_tools:?}"
        );
    }
    // 裸名 Web / Artifact 提供面已删除：任何装配面都不再出现。
    for gone in ["WebFetch", "WebSearch", "artifact"] {
        assert!(
            !enabled_tools.iter().any(|name| name == gone),
            "裸名 {gone} 不应出现在链工具集合: {enabled_tools:?}"
        );
    }

    let mut disabled = base_context();
    disabled.mcp_pool = Some(pool_with_builtin_instances());
    disabled
        .meta_harness_disabled
        .insert("ArtifactMiddleware".to_string());
    let disabled_tools = assemble_tool_names(&disabled);
    assert!(!disabled_tools
        .iter()
        .any(|name| name == "mcp__artifact__artifact"));
    assert!(
        disabled_tools
            .iter()
            .any(|name| name == "mcp__web__WebSearch"),
        "关闭 artifact 不影响 web 实例: {disabled_tools:?}"
    );
    for expected in ["SearchExtraTools", "ExecuteExtraTool"] {
        assert!(disabled_tools.iter().any(|name| name == expected));
    }
}

/// 权限模式不影响链组成与 Permission/AskUser 位置（四种模式一致）。
#[test]
fn permission_mode_keeps_chain_shape() {
    for mode in [
        PermissionMode::Default,
        PermissionMode::AcceptEdit,
        PermissionMode::AutoMode,
        PermissionMode::Bypass,
    ] {
        let mut ctx = base_context();
        ctx.permission_mode = SharedPermissionMode::new(mode);
        let names = assemble_names(&ctx);
        // 位置随 Web / Artifact 两槽位删除各前移 1（A7/A14 期望值同步）。
        assert_eq!(
            names.iter().position(|n| n == "HumanInTheLoopMiddleware"),
            Some(16),
            "mode {mode:?}: AskUser 位置漂移"
        );
        assert_eq!(
            names.iter().position(|n| n == "PermissionMiddleware"),
            Some(15),
            "mode {mode:?}: Permission 位置漂移"
        );
        // 条件中间件（Hook/MCP/Workflow/LSP/Goal）不应出现
        for cond in [
            "HookMiddleware",
            "McpMiddleware",
            "WorkflowMiddleware",
            "LspMiddleware",
            "GoalMiddleware",
        ] {
            assert!(
                !names.contains(&cond.to_string()),
                "mode {mode:?}: 不应注册 {cond}"
            );
        }
    }
}

/// Hook 组非空 → 每组展开一个 HookMiddleware，插在 Cron 之后、HITL 之前。
#[test]
fn hook_groups_expand_hook_middleware() {
    let mut ctx = base_context();
    ctx.hook_groups = vec![vec![make_hook()], vec![make_hook(), make_hook()], vec![]];
    let names = assemble_names(&ctx);
    // 空组不展开；非空组各展开一个实例
    assert_eq!(
        names
            .iter()
            .filter(|n| n.as_str() == "HookMiddleware")
            .count(),
        2
    );
    let pos_cron = names.iter().position(|n| n == "CronMiddleware").unwrap();
    let pos_hook1 = names.iter().position(|n| n == "HookMiddleware").unwrap();
    let pos_hitl = names
        .iter()
        .position(|n| n == "HumanInTheLoopMiddleware")
        .unwrap();
    assert!(
        pos_cron < pos_hook1 && pos_hook1 < pos_hitl,
        "Hook 组位置错误: {names:?}"
    );
}

#[test]
fn dynamic_mcp_registers_deferred_control_tool_at_mcp_slot() {
    let mut ctx = base_context();
    ctx.dynamic_mcp = Some(Arc::new(FakeDynamicDeployment));

    let names = assemble_names(&ctx);
    let tools = assemble_tool_names(&ctx);

    assert!(names.contains(&"DynamicMcpMiddleware".to_string()));
    assert!(tools.contains(&"DynamicMCP".to_string()));
    assert!(!tools
        .iter()
        .any(|tool| matches!(tool.as_str(), "DynamicMCP.load" | "DynamicMCP.unload")));
}

#[test]
fn dynamic_mcp_projection_is_bound_without_optional_registries() {
    let mut ctx = base_context();
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    ctx.dynamic_mcp = Some(Arc::new(FakeDynamicDeployment));
    assert!(ctx.mcp_skill_registry.is_none());
    assert!(ctx.command_registry.is_none());
    let projection = Arc::clone(&ctx.dynamic_mcp_projection);

    let names = assemble_names(&ctx);
    let names_again = assemble_names(&ctx);

    assert!(names.contains(&"McpMiddleware".to_string()));
    assert!(names_again.contains(&"McpMiddleware".to_string()));
    assert!(projection.lock().is_some());
}

/// 条件注册矩阵：MCP / Workflow / LSP / Goal 开关组合。
#[test]
fn conditional_registration_matrix() {
    // 单独开启
    let mut with_mcp = base_context();
    with_mcp.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    let names_mcp = assemble_names(&with_mcp);
    let pos_mcp = names_mcp.iter().position(|n| n == "McpMiddleware").unwrap();
    let pos_sub = names_mcp
        .iter()
        .position(|n| n == "SubAgentMiddleware")
        .unwrap();
    let pos_ts = names_mcp.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(
        pos_sub < pos_mcp && pos_mcp < pos_ts,
        "MCP 位置错误: {names_mcp:?}"
    );

    let mut with_wf = base_context();
    with_wf.workflow_executor = Some(Arc::new(FakeAgentExecutor));
    let names_wf = assemble_names(&with_wf);
    let pos_wf = names_wf
        .iter()
        .position(|n| n == "WorkflowMiddleware")
        .unwrap();
    let pos_sub_wf = names_wf
        .iter()
        .position(|n| n == "SubAgentMiddleware")
        .unwrap();
    let pos_ts_wf = names_wf.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(
        pos_sub_wf < pos_wf && pos_wf < pos_ts_wf,
        "Workflow 位置错误: {names_wf:?}"
    );

    let mut with_lsp = base_context();
    with_lsp.lsp_servers = vec![make_lsp_config()];
    let names_lsp = assemble_names(&with_lsp);
    let pos_lsp = names_lsp.iter().position(|n| n == "LspMiddleware").unwrap();
    let pos_ts_lsp = names_lsp.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(pos_ts_lsp < pos_lsp, "LSP 位置错误: {names_lsp:?}");

    let mut with_goal = base_context();
    with_goal.goal_controller = Some(Arc::new(FakeGoalController));
    let names_goal = assemble_names(&with_goal);
    assert_eq!(
        names_goal.last().map(String::as_str),
        Some("GoalMiddleware")
    );
}

/// 会话级 LSP pool 端口注入 → 装配走 downcast 复用分支（H1），
/// LspMiddleware 照常注册且位置不变（与临时实例路径一致）。
#[test]
fn lsp_pool_port_injected_registers_middleware() {
    let mut ctx = base_context();
    ctx.lsp_servers = vec![make_lsp_config()];
    ctx.lsp_pool = create_session_lsp_pool("/tmp/contract-test", &ctx.lsp_servers);
    assert!(ctx.lsp_pool.is_some(), "有配置时工厂应返回端口");

    let names = assemble_names(&ctx);
    let pos_lsp = names.iter().position(|n| n == "LspMiddleware").unwrap();
    let pos_ts_lsp = names.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(pos_ts_lsp < pos_lsp, "LSP 位置错误: {names:?}");
}

/// 无 LSP 配置时工厂返回 None（不注册 LSP 中间件，条件注册语义一致）。
#[test]
fn lsp_pool_factory_empty_config_returns_none() {
    assert!(create_session_lsp_pool("/tmp", &[]).is_none());
}

/// H5：无插件但全局 settings.json 存在 `config.lspServers` 时，合并结果
/// 非空且 source 标记为 Global；装配级验证——会话级 pool 非空、
/// 链上注册 LspMiddleware（此前无插件时 LSP 产品线静默不可用）。
#[test]
fn merged_lsp_servers_global_without_plugins_registers_middleware() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"config":{"lspServers":{"rust-analyzer":{"command":"rust-analyzer"}}}}"#,
    )
    .unwrap();

    let merged = load_merged_lsp_servers(&settings, Vec::new());
    assert_eq!(merged.len(), 1, "全局配置应单独生效");
    let server = &merged[0];
    assert_eq!(server.name, "rust-analyzer");
    assert!(
        matches!(server.source, Some(LspConfigSource::Global(ref p)) if p == &settings),
        "全局来源应标记 Global: {:?}",
        server.source
    );

    // 装配级：合并结果 → 会话级 pool → 链上注册 LspMiddleware
    let mut ctx = base_context();
    ctx.lsp_servers = merged.clone();
    ctx.lsp_pool = create_session_lsp_pool("/tmp/contract-test", &ctx.lsp_servers);
    assert!(ctx.lsp_pool.is_some(), "全局配置存在时工厂应返回端口");
    let names = assemble_names(&ctx);
    assert!(
        names.iter().any(|n| n == "LspMiddleware"),
        "无插件但全局配置存在时 LspMiddleware 应注册: {names:?}"
    );
}

/// H5：合并方向对齐 MCP（global < plugin）——同名 key 插件覆盖全局。
#[test]
fn merged_lsp_servers_plugin_overrides_global() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"config":{"lspServers":{"same":{"command":"global-bin"}}}}"#,
    )
    .unwrap();

    let plugin = LspServerConfig {
        name: "same".to_string(),
        command: "plugin-bin".to_string(),
        ..make_lsp_config()
    };
    let merged = load_merged_lsp_servers(&settings, vec![plugin]);
    assert_eq!(merged.len(), 1, "同名 key 应合并为一条");
    assert_eq!(merged[0].command, "plugin-bin", "插件应覆盖全局");
}

/// H5：settings.json 不存在或无 `lspServers` 字段时返回空 Vec
/// （装配处 `lsp_servers.is_empty()` 条件注册语义不变）。
#[test]
fn merged_lsp_servers_empty_without_global_config() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.json");
    assert!(load_merged_lsp_servers(&missing, Vec::new()).is_empty());

    let no_lsp = temp.path().join("settings.json");
    std::fs::write(&no_lsp, r#"{"config":{"mcpServers":{}}}"#).unwrap();
    assert!(load_merged_lsp_servers(&no_lsp, Vec::new()).is_empty());
}

/// 全开组合：完整序列精确断言（Hook 2 组 + MCP + Workflow + LSP + Goal）。
#[test]
fn full_config_chain_order() {
    let mut ctx = base_context();
    ctx.hook_groups = vec![vec![make_hook()], vec![make_hook()]];
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    ctx.workflow_executor = Some(Arc::new(FakeAgentExecutor));
    ctx.lsp_servers = vec![make_lsp_config()];
    ctx.goal_controller = Some(Arc::new(FakeGoalController));

    let names = assemble_names(&ctx);
    assert_eq!(
        names,
        vec![
            "DefaultSystemPromptMiddleware",
            "LangMiddleware",
            "AgentsMdMiddleware",
            "AgentDefineMiddleware",
            "PluginMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "AtMentionMiddleware",
            "ImageMiddleware",
            "FilesystemMiddleware",
            "GitAttributionMiddleware",
            "GitWatchMiddleware",
            "TerminalMiddleware",
            "TodoMiddleware",
            "CronMiddleware",
            "HookMiddleware",
            "HookMiddleware",
            "PermissionMiddleware",
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "McpMiddleware",
            "WorkflowMiddleware",
            "PtcMiddleware",
            "ToolSearch",
            "LspMiddleware",
            "GoalMiddleware",
        ]
    );
}

#[test]
fn workflow_agent_type_uses_project_definition_before_built_in() {
    let temp = tempfile::tempdir().unwrap();
    let agents_dir = temp.path().join(".claude/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("explorer.md"),
        "---\nname: explorer\ndescription: Project override\ntools: Read, Grep\ndisallowedTools: Grep\nmodel: opus\nmaxTurns: 7\nskills: [research]\n---\n\nProject explorer persona.",
    )
    .unwrap();

    let factory = default_workflow_middleware_factory();
    let definition = factory
        .resolve_agent_definition("explorer", temp.path().to_str().unwrap())
        .unwrap();

    assert_eq!(definition.model.as_deref(), Some("opus"));
    assert_eq!(
        definition.allowed_tools,
        Some(vec!["Read".into(), "Grep".into()])
    );
    assert_eq!(definition.disallowed_tools, vec!["Grep"]);
    assert_eq!(definition.skill_names, vec!["research"]);
    assert_eq!(definition.max_iterations, 7);
    assert_eq!(
        definition
            .prompt_overrides
            .as_ref()
            .and_then(|overrides| overrides.persona.as_deref()),
        Some("Project explorer persona.")
    );
}

#[test]
fn workflow_plan_definition_inherits_model_and_preserves_sandbox_write_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let definition = default_workflow_middleware_factory()
        .resolve_agent_definition("plan", temp.path().to_str().unwrap())
        .unwrap();

    assert_eq!(definition.model, None);
    assert_eq!(definition.allowed_write_dirs, vec![".peri/plans/"]);
    assert!(definition
        .disallowed_tools
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case("Write")));
}

#[test]
fn workflow_agent_type_rejects_unknown_definition() {
    let temp = tempfile::tempdir().unwrap();
    let error = default_workflow_middleware_factory()
        .resolve_agent_definition("does-not-exist", temp.path().to_str().unwrap())
        .unwrap_err();

    assert!(error.contains("cannot find agent definition 'does-not-exist'"));
}

/// [回归测试] workflow 真实 executor 在 Reason 流中取消时，
/// 必须返回 interrupted，不得将 stage-local cancel 降级成 runagent-threw。
#[tokio::test]
async fn test_workflow_executor_cancel_during_model_stream_is_interrupted() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let model: Arc<dyn Model> = Arc::new(CancelGateModel {
        entered: std::sync::Mutex::new(Some(entered_tx)),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut ctx = workflow_context_with_disabled(&[]);
    ctx.cancel = Some(cancel.clone());
    ctx.model_factory = Arc::new(move |_model, _max_tokens, _observer| {
        peri_agent::agent::workflow::WorkflowModel {
            model: Arc::clone(&model),
            model_name: "cancel-gate".to_string(),
            tier: None,
        }
    });
    let executor = peri_agent::agent::workflow::WorkflowAgentExecutor::new(ctx);
    let task = tokio::spawn(async move {
        executor
            .execute(AgentRunParams {
                run_id: "cancel-workflow-run".to_string(),
                agent_id: 1,
                prompt: "wait for cancellation".to_string(),
                schema: None,
                model: None,
                max_tokens: None,
                agent_type: None,
                isolation: None,
                allowed_tools: None,
                label: None,
                phase: None,
            })
            .await
    });
    entered_rx.await.expect("workflow model stream 必须已返回");

    cancel.cancel();
    let result = task.await.expect("workflow executor task 不得 panic");

    match result {
        AgentRunResult::Dead { reason, detail } => {
            assert_eq!(reason.as_deref(), Some("interrupted"));
            assert!(
                detail
                    .as_deref()
                    .is_some_and(|text| text.contains("interrupted")),
                "workflow 取消详情必须明确: {detail:?}"
            );
        }
        other => panic!("workflow 取消必须返回 Dead(interrupted): {other:?}"),
    }
}

#[tokio::test]
async fn test_workflow_executor_forwarder_join_error_is_dead_and_failed_telemetry() {
    let model: Arc<dyn Model> = Arc::new(CompletedWorkflowModel);
    let telemetry = Arc::new(std::sync::Mutex::new(Vec::new()));
    let telemetry_for_hook = Arc::clone(&telemetry);
    let mut ctx = workflow_context_with_disabled(&[]);
    ctx.model_factory = Arc::new(move |_model, _max_tokens, _observer| {
        peri_agent::agent::workflow::WorkflowModel {
            model: Arc::clone(&model),
            model_name: "workflow-complete".to_string(),
            tier: None,
        }
    });
    ctx.forwarder_launcher = Arc::new(|_, _, _| {
        let handle = tokio::spawn(std::future::pending());
        handle.abort();
        handle
    });
    ctx.langfuse_hooks = Some(peri_agent::session::exec::executor::LangfuseHooks {
        on_turn_start: Arc::new(|_| {}),
        on_turn_end: Arc::new(move |outcome| {
            telemetry_for_hook.lock().unwrap().push(outcome);
            None
        }),
        bridge_factory: Arc::new(|_, _| None),
    });

    let result = peri_agent::agent::workflow::WorkflowAgentExecutor::new(ctx)
        .execute(AgentRunParams {
            run_id: "forwarder-failure-workflow-run".to_string(),
            agent_id: 1,
            prompt: "complete before forwarder failure".to_string(),
            schema: None,
            model: None,
            max_tokens: None,
            agent_type: None,
            isolation: None,
            allowed_tools: None,
            label: None,
            phase: None,
        })
        .await;

    assert!(matches!(
        result,
        AgentRunResult::Dead { reason: Some(reason), .. }
            if reason == "event-forwarder-failed"
    ));
    let outcomes = telemetry.lock().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        &outcomes[0],
        peri_acp_types::session::TurnTelemetryOutcome::Failed { failure }
            if failure.kind == peri_acp_types::session::ExecutionFailureKind::Internal
    ));
}

// ─── MetaHarness（设计 §2.5）：middleware 关闭契约测试 ────────────────────────

use peri_acp_types::meta_harness::{MIDDLEWARE_NAMES, MIDDLEWARE_TOOL_NAMES};
use peri_agent::agent::workflow::WorkflowAgentContext;

/// 装配并返回链上中间件 collect_tools 的工具名集合。
fn assemble_tool_names(ctx: &AssemblyContext) -> Vec<String> {
    let out = build_middleware_chain(&ProductionChainAssembler, ctx);
    out.chain
        .collect_tools(&ctx.cwd)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect()
}

/// 每个已知 middleware 名单独禁用：链上不出现、空 disabled 时完整链序不变。
#[test]
fn meta_harness_disables_each_known_middleware() {
    let baseline = assemble_names(&base_context());
    for name in MIDDLEWARE_NAMES {
        let mut ctx = base_context();
        ctx.meta_harness_disabled.insert(name.to_string());
        let names = assemble_names(&ctx);
        assert!(
            !names.iter().any(|n| n == name),
            "disabled {name} 后仍出现在链上: {names:?}"
        );
    }
    // 空 disabled 与默认配置完全一致（default_config_produces_canonical_chain 的
    // 基线由本断言再次锁定，防止过滤逻辑误伤未禁用 middleware）。
    assert_eq!(assemble_names(&base_context()), baseline);
}

/// 条件注册 middleware：即使运行条件满足，disabled 后也不构造（构造副作用
/// 语义——不构造实例、不设置 notifier）。
#[test]
fn meta_harness_disables_conditional_middleware_despite_conditions() {
    // MCP：pool 存在 + disabled → 不注册（notifier 不设置）
    let mut ctx = base_context();
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    ctx.meta_harness_disabled
        .insert("McpMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "McpMiddleware"), "{names:?}");

    // Workflow：executor 存在 + disabled → 不注册 adaptor
    let mut ctx = base_context();
    ctx.workflow_executor = Some(Arc::new(FakeAgentExecutor));
    ctx.meta_harness_disabled
        .insert("WorkflowMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(
        !names.iter().any(|n| n == "WorkflowMiddleware"),
        "{names:?}"
    );

    // LSP：配置存在 + disabled → 不注册
    let mut ctx = base_context();
    ctx.lsp_servers = vec![make_lsp_config()];
    ctx.meta_harness_disabled
        .insert("LspMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "LspMiddleware"), "{names:?}");

    // Goal：controller 存在 + disabled → 不注册
    let mut ctx = base_context();
    ctx.goal_controller = Some(Arc::new(FakeGoalController));
    ctx.meta_harness_disabled
        .insert("GoalMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "GoalMiddleware"), "{names:?}");

    // Hook：hook group 存在 + disabled → 全部组不展开
    let mut ctx = base_context();
    ctx.hook_groups = vec![vec![make_hook()], vec![make_hook()]];
    ctx.meta_harness_disabled
        .insert("HookMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "HookMiddleware"), "{names:?}");
}

/// A6/R23 的关闭矩阵：四种输入 × 四个可观察面。
///
/// 输入：`WebMiddleware=false` / `ArtifactMiddleware=false` / 两者都 false /
/// `McpMiddleware=false`。四个面：① direct 能力面（模型可见的直连工具）、
/// ② deferred 目录面（ToolSearch 摘要与检索的来源）、③ subagent `parent_tools`、
/// ④ workflow agent 工具列表。断言全部落在可观察能力面，不依赖中间量。
#[test]
fn builtin_capability_closure_matrix_covers_all_faces() {
    use crate::subagent::SubAgentMiddleware;

    let all_builtin: Vec<&'static str> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .map(|declaration| declaration.effective_name)
        .collect();
    let all_declarations: Vec<_> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .collect();

    // (关闭的实例名列表, 进入 disabled 的策略键列表)
    let both_keys: Vec<&str> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .map(|instance| instance.policy_key)
        .collect();
    let cases: Vec<(Vec<&'static str>, Vec<&str>)> = {
        let mut cases = Vec::new();
        for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
            cases.push((
                instance
                    .tools
                    .iter()
                    .map(|declaration| declaration.effective_name)
                    .collect::<Vec<_>>(),
                vec![instance.policy_key],
            ));
        }
        cases.push((all_builtin.clone(), both_keys.clone()));
        cases
    };

    for (closed_tools, policy_keys) in &cases {
        let disabled: std::collections::HashSet<String> =
            policy_keys.iter().map(|key| key.to_string()).collect();
        assert!(!disabled.is_empty(), "关闭输入必须映射到至少一个策略键");

        let mut ctx = base_context();
        ctx.mcp_pool = Some(pool_with_builtin_instances());
        ctx.meta_harness_disabled = disabled.clone();

        // 面①/②：链工具集合里不得残留该实例的 bridge（direct 与 deferred 都不行）。
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let collected: Vec<(String, bool)> = out
            .chain
            .collect_tools(&ctx.cwd)
            .into_iter()
            .map(|tool| (tool.name().to_string(), tool.is_direct()))
            .collect();
        let collected_names: Vec<&str> = collected.iter().map(|(name, _)| name.as_str()).collect();
        let survivors: Vec<&str> = all_builtin
            .iter()
            .copied()
            .filter(|tool| !closed_tools.contains(tool))
            .collect();
        for tool in closed_tools {
            assert!(
                !collected.iter().any(|(name, _)| name == tool),
                "[{policy_keys:?}] 关闭后能力面①/②不得含 {tool}: {collected:?}"
            );
        }
        for tool in &survivors {
            let entry = collected.iter().find(|(name, _)| name == tool);
            assert!(
                entry.is_some(),
                "[{policy_keys:?}] 未关闭实例的 {tool} 必须仍在能力面①/②: {collected:?}"
            );
            assert!(
                entry.is_some_and(|(_, direct)| *direct),
                "[{policy_keys:?}] 未关闭实例的 {tool} 必须是 direct（IF-D13）: {collected:?}"
            );
        }

        // 面③：subagent parent_tools。
        let subagent = out
            .subagent_mw
            .expect("矩阵需要 SubAgentMiddleware")
            .downcast_arc::<SubAgentMiddleware>()
            .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
        let parent_tool = subagent.build_tool("/tmp/contract-test");
        let parent_names: Vec<&str> = parent_tool.parent_tools.iter().map(|t| t.name()).collect();
        for tool in closed_tools {
            assert!(
                !parent_names.contains(tool),
                "[{policy_keys:?}] parent_tools（面③）不得含 {tool}: {parent_names:?}"
            );
        }
        for tool in &survivors {
            assert!(
                parent_names.contains(tool),
                "[{policy_keys:?}] parent_tools（面③）应保留 {tool}: {parent_names:?}"
            );
        }

        // 面④：workflow agent 工具列表（生产入口的带池工厂）。
        let workflow_tools =
            default_workflow_middleware_factory_with_pool(Some(pool_with_builtin_instances()))
                .build_tools("/tmp/contract-test", &disabled, None);
        let workflow_names: Vec<&str> = workflow_tools.iter().map(|t| t.name()).collect();
        for tool in closed_tools {
            assert!(
                !workflow_names.contains(tool),
                "[{policy_keys:?}] workflow agent 工具列表（面④）不得含 {tool}: {workflow_names:?}"
            );
        }
        for tool in &survivors {
            assert!(
                workflow_names.contains(tool),
                "[{policy_keys:?}] workflow agent 工具列表（面④）应保留 {tool}: {workflow_names:?}"
            );
        }

        // 三个面都不得残留裸名 Web / Artifact 工具（A6/IF-D8：能力改由 effective
        // name 的 builtin bridge 提供，不再有 middleware 提供面）。裸名从声明表
        // 派生，不硬编码。
        for declaration in &all_declarations {
            let bare = declaration.original_name;
            for (face, names) in [
                ("面①/②链工具", &collected_names),
                ("面③parent_tools", &parent_names),
                ("面④workflow", &workflow_names),
            ] {
                assert!(
                    !names.contains(&bare),
                    "[{policy_keys:?}] {face} 不得含裸名 {bare}: {names:?}"
                );
            }
        }
    }

    // `McpMiddleware=false`：整个 MCP 槽位不构造 —— 主链与 workflow 面都不该出现
    // 任何 MCP 工具（builtin 能力随槽位关闭，而不是「天然关闭」）。
    let mut mcp_off = base_context();
    mcp_off.mcp_pool = Some(pool_with_builtin_instances());
    mcp_off
        .meta_harness_disabled
        .insert("McpMiddleware".to_string());
    let collected_off = assemble_tool_names(&mcp_off);
    assert!(
        !collected_off.iter().any(|name| name.starts_with("mcp__")),
        "McpMiddleware 关闭后主链不得含 MCP 工具: {collected_off:?}"
    );
    let workflow_off =
        default_workflow_middleware_factory_with_pool(Some(pool_with_builtin_instances()))
            .build_tools(
                "/tmp/contract-test",
                &["McpMiddleware".to_string()].into_iter().collect(),
                None,
            );
    assert!(
        !workflow_off
            .iter()
            .any(|tool| tool.name().starts_with("mcp__")),
        "McpMiddleware 关闭后 workflow 面不得含 MCP 工具: {:?}",
        workflow_off.iter().map(|t| t.name()).collect::<Vec<_>>()
    );
}

/// SubAgentMiddleware 关闭 → 关联构造联动置空（parent_tools 不注入、
/// subagent_mw 槽位 None、链上不注册、SubAgent 工具消失——禁止半开状态）。
#[test]
fn meta_harness_disables_subagent_middleware_fully() {
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SubAgentMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);

    assert!(
        out.subagent_mw.is_none(),
        "SubAgentMiddleware 关闭后 subagent_mw 槽位必须为 None"
    );
    let names: Vec<&str> = out.chain.names();
    assert!(
        !names.contains(&"SubAgentMiddleware"),
        "链上不应出现 SubAgentMiddleware: {names:?}"
    );
    let tool_names: Vec<String> = out
        .chain
        .collect_tools(&ctx.cwd)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        !tool_names
            .iter()
            .any(|n| n == "Agent" || n == "AgentResult"),
        "SubAgent 工具不应出现: {tool_names:?}"
    );
}

/// 工具连坐语义：关闭持有 middleware 后其全部工具从链收集结果消失。
///
/// Web / Artifact 已迁移为 builtin 实例（A6/A7）：对应的关闭键走注册表
/// `policy_key` → `closed_instances` 的**同一份**映射，断言落在可观察能力面
/// （链工具集合），不再依赖 middleware 连坐。
#[test]
fn meta_harness_disabled_tools_removed_from_chain() {
    let cases: &[(&str, &[&str])] = &[
        (
            "FilesystemMiddleware",
            &["Read", "Write", "Edit", "Glob", "Grep", "folder_operations"],
        ),
        ("TerminalMiddleware", &["Bash"]),
        ("SkillsMiddleware", &["SkillTool", "DiscoverSkillsTool"]),
        ("SubAgentMiddleware", &["Agent", "AgentResult"]),
    ];
    for (mw, expected_gone) in cases {
        let mut ctx = base_context();
        ctx.meta_harness_disabled.insert(mw.to_string());
        let tool_names = assemble_tool_names(&ctx);
        for tool in *expected_gone {
            assert!(
                !tool_names.iter().any(|n| n == tool),
                "disabled {mw} 后工具 {tool} 仍可见: {tool_names:?}"
            );
        }
    }

    // builtin 实例：关闭键按注册表 policy_key 命中，工具面同时归零。
    for (policy_key, gone) in [
        (
            "WebMiddleware",
            vec!["mcp__web__WebSearch", "mcp__web__WebFetch"],
        ),
        ("ArtifactMiddleware", vec!["mcp__artifact__artifact"]),
    ] {
        let mut ctx = base_context();
        ctx.mcp_pool = Some(pool_with_builtin_instances());
        ctx.meta_harness_disabled.insert(policy_key.to_string());
        let tool_names = assemble_tool_names(&ctx);
        for tool in &gone {
            assert!(
                !tool_names.iter().any(|n| n == tool),
                "disabled {policy_key} 后 builtin 工具 {tool} 仍可见: {tool_names:?}"
            );
        }
        // 另一实例不受影响。
        let survivor = match policy_key {
            "WebMiddleware" => "mcp__artifact__artifact",
            _ => "mcp__web__WebSearch",
        };
        assert!(
            tool_names.iter().any(|n| n == survivor),
            "关闭 {policy_key} 不得影响 {survivor}: {tool_names:?}"
        );
    }
}

/// 提问通道连坐语义：关闭新 HumanInTheLoopMiddleware 后 AskUserQuestion
/// 从链收集结果消失（2026-08-15 拆分后纳入关闭面，原"始终注册"测试反转）。
#[test]
fn meta_harness_ask_user_tool_follows_hitl_disabled() {
    // 全开：AskUserQuestion 在工具集中
    let mut ctx = base_context();
    let tool_names = assemble_tool_names(&ctx);
    assert!(
        tool_names.iter().any(|n| n == "AskUserQuestion"),
        "全开时 AskUserQuestion 应可见"
    );

    // 关闭 HumanInTheLoopMiddleware：AskUserQuestion 消失；其余不变
    ctx.meta_harness_disabled
        .insert("HumanInTheLoopMiddleware".to_string());
    let tool_names = assemble_tool_names(&ctx);
    assert!(
        !tool_names.iter().any(|n| n == "AskUserQuestion"),
        "关闭 HumanInTheLoopMiddleware 后 AskUserQuestion 应消失: {tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|n| n == "Bash"),
        "关闭提问通道不影响其他工具: {tool_names:?}"
    );

    // 关闭 PermissionMiddleware 不影响 AskUserQuestion（审批/提问独立开关）
    let mut ctx2 = base_context();
    ctx2.meta_harness_disabled
        .insert("PermissionMiddleware".to_string());
    let tool_names = assemble_tool_names(&ctx2);
    assert!(
        tool_names.iter().any(|n| n == "AskUserQuestion"),
        "关闭 PermissionMiddleware 后 AskUserQuestion 应保留"
    );

    // 宿主级 shared_tools 不再注册任何工具（生产路径写入点归零）
    build_middleware_chain(&ProductionChainAssembler, &ctx2);
    assert!(
        !ctx2.shared_tools.read().contains_key("AskUserQuestion"),
        "shared_tools 不应再注册 AskUserQuestion（移入链 collect_tools）"
    );
}

/// parent_tools（子 agent 继承工具）按持有 middleware 分支过滤：
/// 关闭 Filesystem/Terminal 后 SubAgent 继承工具中无对应工具；
/// builtin 实例按 A6 面② 以 **direct** bridge 保留（迁移后子 agent 仍能用
/// `mcp__web__*`），关闭后归零。
#[test]
fn meta_harness_disabled_parent_tools_filtered() {
    use crate::subagent::SubAgentMiddleware;

    let cases: &[(&str, &[&str])] = &[
        (
            "FilesystemMiddleware",
            &["Read", "Write", "Edit", "Glob", "Grep", "folder_operations"],
        ),
        ("TerminalMiddleware", &["Bash"]),
    ];
    for (mw, expected_gone) in cases {
        let mut ctx = base_context();
        ctx.meta_harness_disabled.insert(mw.to_string());
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let subagent = out
            .subagent_mw
            .expect("parent_tools 过滤测试需要 SubAgentMiddleware")
            .downcast_arc::<SubAgentMiddleware>()
            .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
        let tool = subagent.build_tool("/tmp/contract-test");
        let parent_names: Vec<&str> = tool.parent_tools.iter().map(|t| t.name()).collect();
        for tool_name in *expected_gone {
            assert!(
                !parent_names.contains(tool_name),
                "disabled {mw} 后 parent_tools 仍含 {tool_name}: {parent_names:?}"
            );
        }
    }

    // 面②：未关闭时 parent_tools 含 builtin 的 effective name 且 `is_direct()`。
    let mut ctx = base_context();
    ctx.mcp_pool = Some(pool_with_builtin_instances());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let subagent = out
        .subagent_mw
        .expect("parent_tools 断言需要 SubAgentMiddleware")
        .downcast_arc::<SubAgentMiddleware>()
        .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
    let tool = subagent.build_tool("/tmp/contract-test");
    let declared: Vec<(&str, bool)> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .map(|declaration| (declaration.effective_name, declaration.direct))
        .collect();
    for (name, direct) in &declared {
        let entry = tool
            .parent_tools
            .iter()
            .find(|parent| parent.name() == *name)
            .unwrap_or_else(|| {
                panic!(
                    "parent_tools 应含 builtin 工具 {name}: {:?}",
                    tool.parent_tools
                        .iter()
                        .map(|t| t.name())
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            entry.is_direct(),
            *direct,
            "parent_tools 中的 {name} 直连性必须等于注册表声明（IF-D13，A6 面②）"
        );
    }
    // 裸名提供面已删除：任何装配面都不再出现。
    assert!(!tool
        .parent_tools
        .iter()
        .any(|parent| { matches!(parent.name(), "WebFetch" | "WebSearch" | "artifact") }));

    // 关闭 Web 实例：只影响该实例的 bridge（面②的关闭过滤）。
    let mut closed_ctx = base_context();
    closed_ctx.mcp_pool = Some(pool_with_builtin_instances());
    closed_ctx
        .meta_harness_disabled
        .insert("WebMiddleware".to_string());
    let closed_out = build_middleware_chain(&ProductionChainAssembler, &closed_ctx);
    let closed_subagent = closed_out
        .subagent_mw
        .expect("parent_tools 关闭断言需要 SubAgentMiddleware")
        .downcast_arc::<SubAgentMiddleware>()
        .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
    let closed_tool = closed_subagent.build_tool("/tmp/contract-test");
    let closed_names: Vec<&str> = closed_tool.parent_tools.iter().map(|t| t.name()).collect();
    assert!(
        !closed_names
            .iter()
            .any(|name| name.starts_with("mcp__web__")),
        "关闭 WebMiddleware 后 parent_tools 不得含 web 实例工具: {closed_names:?}"
    );
    assert!(
        closed_names.contains(&"mcp__artifact__artifact"),
        "关闭 WebMiddleware 不得影响 artifact 实例: {closed_names:?}"
    );

    // SubAgentMiddleware 关闭 → parent_tools 完全不构造（subagent_mw 槽位 None）
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SubAgentMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    assert!(
        out.subagent_mw.is_none(),
        "SubAgentMiddleware 关闭后 parent_tools 不应注入（槽位联动置空）"
    );
}

/// 「已知键全集」既不缺项也不重复（A7 的三条同时成立）。
///
/// 1. 链槽位名 == `MIDDLEWARE_NAMES` 去掉 builtin 实例策略键后的集合；
/// 2. builtin 策略键集合 == 声明表 `policy_key` 集合（常量或声明表漂移即红）；
/// 3. 槽位名 ∩ 策略键 == ∅（两表语义不重叠）。
///
/// 与 §3 IF-D7 A 节的等价表述一致：`槽位名 ∪ 策略键 == MIDDLEWARE_NAMES ∪
/// BUILTIN_INSTANCE_POLICY_KEYS`。`BUILTIN_INSTANCE_POLICY_KEYS` 常量由 S-02
/// （W3，`meta_harness.rs`）引入——本断言用声明表派生同一集合，因此在
/// 「I-03 → S-02」两个时点都成立且强度不降。
#[test]
fn middleware_names_match_production_blueprint() {
    let blueprint = production_blueprint();
    let slot_names: std::collections::HashSet<&str> =
        blueprint.iter().map(slot_middleware_name).collect();
    let policy_keys: std::collections::HashSet<&str> =
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .map(|instance| instance.policy_key)
            .collect();
    let const_names: std::collections::HashSet<&str> = MIDDLEWARE_NAMES.iter().copied().collect();

    assert!(
        !policy_keys.is_empty(),
        "builtin 实例策略键不得为空（关闭语义的来源）"
    );
    assert_eq!(
        policy_keys.len(),
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES.len(),
        "policy_key 必须在声明表内唯一"
    );
    assert!(
        slot_names.is_disjoint(&policy_keys),
        "链槽位名与 builtin 策略键必须语义不重叠: {slot_names:?} / {policy_keys:?}"
    );
    // 1：MIDDLEWARE_NAMES 去掉策略键后必须恰为链槽位名。
    let const_slots: std::collections::HashSet<&str> =
        const_names.difference(&policy_keys).copied().collect();
    assert_eq!(
        slot_names, const_slots,
        "MIDDLEWARE_NAMES 必须恰为「链槽位名 ∪ builtin 策略键」（不缺项、不重复）"
    );
}

/// 槽位 → middleware `name()` 返回值映射（与 assembly.rs 装配分支一一对应）。
fn slot_middleware_name(slot: &ChainSlot) -> &'static str {
    match slot {
        ChainSlot::DefaultSystemPrompt => "DefaultSystemPromptMiddleware",
        ChainSlot::Lang => "LangMiddleware",
        ChainSlot::AgentsMd => "AgentsMdMiddleware",
        ChainSlot::AgentDefine => "AgentDefineMiddleware",
        ChainSlot::Plugin => "PluginMiddleware",
        ChainSlot::Skills => "SkillsMiddleware",
        ChainSlot::SkillPreload => "SkillPreloadMiddleware",
        ChainSlot::AtMention => "AtMentionMiddleware",
        ChainSlot::Image => "ImageMiddleware",
        ChainSlot::Filesystem => "FilesystemMiddleware",
        ChainSlot::GitAttribution => "GitAttributionMiddleware",
        ChainSlot::GitWatch => "GitWatchMiddleware",
        ChainSlot::Terminal => "TerminalMiddleware",
        ChainSlot::Todo => "TodoMiddleware",
        ChainSlot::Cron => "CronMiddleware",
        ChainSlot::Hook => "HookMiddleware",
        ChainSlot::Permission => "PermissionMiddleware",
        ChainSlot::AskUser => "HumanInTheLoopMiddleware",
        ChainSlot::SubAgent => "SubAgentMiddleware",
        ChainSlot::Mcp => "McpMiddleware",
        ChainSlot::Workflow => "WorkflowMiddleware",
        ChainSlot::Ptc => "PtcMiddleware",
        ChainSlot::ToolSearch => "ToolSearch",
        ChainSlot::Lsp => "LspMiddleware",
        ChainSlot::Goal => "GoalMiddleware",
    }
}

/// 「常量去掉三个已迁移裸名」== 各 middleware 静态工具名并集。
///
/// 工具名漂移即防御剔除面失真（新增 middleware 工具须同步两处）。
///
/// v4-part-2 W3（A7/IF-D7 B 节）：`WebFetch` / `WebSearch` / `artifact` 已迁移为
/// builtin MCP 实例（`mcp__web__*` / `mcp__artifact__artifact`），不再是任何
/// middleware 的静态工具；`MIDDLEWARE_TOOL_NAMES` 侧的删除归 **S-02**
/// （`peri-acp-types/src/meta_harness.rs`），本文件归 I-03，故断言写成与
/// `middleware_names_match_production_blueprint` 同构的**等价形态**：常量去掉
/// 这三个名字后必须与各 middleware 静态工具名并集相等。S-02 删除前后都成立且
/// 强度不降（删除后 `difference` 为空集，即严格集合相等）。
///
/// 已迁移名从**声明表**派生（不新建第二张反查表，IF-D15 / §9 规则 11）。
#[test]
fn middleware_tool_names_match_static_tool_sets() {
    use crate::middleware::{FilesystemMiddleware, TerminalMiddleware};

    let migrated_bare_names: std::collections::HashSet<&str> =
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .flat_map(|instance| instance.tools.iter())
            .map(|declaration| declaration.original_name)
            .collect();
    assert!(
        !migrated_bare_names.is_empty(),
        "迁移名集合不得为空（本断言的省略面）"
    );

    let static_tools: std::collections::HashSet<&str> = FilesystemMiddleware::tool_names()
        .into_iter()
        .chain(TerminalMiddleware::tool_names())
        .chain(HumanInTheLoopMiddleware::tool_names())
        .chain([
            // SkillsMiddleware
            "SkillTool",
            "DiscoverSkillsTool",
            // SubAgentMiddleware
            "Agent",
            "AgentResult",
            // WorkflowMiddleware（peri-workflow::tool::WorkflowTool）
            "Workflow",
            // TodoMiddleware
            "TodoWrite",
            // ToolSearch
            "ToolSearch",
            "SearchExtraTools",
            "ExecuteExtraTool",
            // LspMiddleware
            "LSP",
            // GoalMiddleware
            "goal",
            // McpMiddleware（静态部分）
            "DiscoverMCP",
            "mcp_read_resource",
        ])
        .collect();
    // Web / Artifact 能力已不在 middleware 提供面上（迁移为 builtin 实例）。
    assert!(
        static_tools.is_disjoint(&migrated_bare_names),
        "已迁移的 Web / Artifact 工具不得再出现在 middleware 静态工具集中: {static_tools:?}"
    );

    let const_tools: std::collections::HashSet<&str> =
        MIDDLEWARE_TOOL_NAMES.iter().copied().collect();
    let const_tools_wo_migrated: std::collections::HashSet<&str> = const_tools
        .difference(&migrated_bare_names)
        .copied()
        .collect();
    assert_eq!(
        static_tools, const_tools_wo_migrated,
        "MIDDLEWARE_TOOL_NAMES（去掉已迁移的三个裸名后）必须与各 middleware 静态工具名并集一致"
    );
}

// ── Workflow agent 链过滤（设计 §2.5 第 3 装配入口）───────────────────────────

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
        session_id: None,
        compact_config: None,
        cancel: None,
        system_prompt: None,
        broker: None,
        permission_mode: None,
        frozen_date: None,
        frozen_language: None,
        thread_store: None,
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

/// Workflow agent 工具列表按 disabled 集合连坐过滤。
///
/// A6 面③：迁移后 workflow agent 的 Web / Artifact 能力来自 builtin 实例的
/// direct bridge，而不是裸名 middleware 工具——因此本用例必须用**带池**的工厂
/// 才能观察到该能力面（无池构造器的行为与迁移前的非 Web 部分逐位一致）。
#[test]
fn workflow_build_tools_filters_disabled() {
    let factory = default_workflow_middleware_factory();
    // 无池构造器：不产生任何 builtin bridge（既有调用点语义不变）。
    let without_pool = factory.build_tools(
        "/tmp/contract-test",
        &std::collections::HashSet::new(),
        None,
    );
    let without_pool_names: Vec<&str> = without_pool.iter().map(|t| t.name()).collect();
    assert!(
        !without_pool_names
            .iter()
            .any(|name| name.starts_with("mcp__")),
        "无池工厂不得凭空产生 MCP 工具: {without_pool_names:?}"
    );

    // 带池工厂：全开时 builtin 工具以 direct 形式进入工具列表。
    let pool_factory =
        default_workflow_middleware_factory_with_pool(Some(pool_with_builtin_instances()));
    let all = pool_factory.build_tools(
        "/tmp/contract-test",
        &std::collections::HashSet::new(),
        None,
    );
    for (name, direct) in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .map(|declaration| (declaration.effective_name, declaration.direct))
    {
        let entry = all
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| {
                panic!(
                    "workflow agent 工具列表应含 builtin 工具 {name}: {:?}",
                    all.iter().map(|t| t.name()).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            entry.is_direct(),
            direct,
            "workflow agent 侧的 {name} 必须保持声明的直连性（A6 面③）"
        );
    }
    let all_names: Vec<&str> = all.iter().map(|t| t.name()).collect();
    for expected in ["Read", "Bash", "SkillTool", "DiscoverSkillsTool"] {
        assert!(
            all_names.contains(&expected),
            "全开时工具 {expected} 应存在: {all_names:?}"
        );
    }
    // 裸名提供面已删除。
    for gone in ["WebFetch", "WebSearch", "artifact"] {
        assert!(
            !all_names.contains(&gone),
            "裸名 {gone} 不得出现在 workflow agent 工具列表: {all_names:?}"
        );
    }

    let cases: &[(&str, &[&str])] = &[
        (
            "FilesystemMiddleware",
            &["Read", "Write", "Edit", "Glob", "Grep"],
        ),
        ("TerminalMiddleware", &["Bash"]),
        ("SkillsMiddleware", &["SkillTool", "DiscoverSkillsTool"]),
    ];
    for (mw, expected_gone) in cases {
        let disabled: std::collections::HashSet<String> = std::iter::once(mw.to_string()).collect();
        let tools = pool_factory.build_tools("/tmp/contract-test", &disabled, None);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for tool in *expected_gone {
            assert!(
                !names.contains(tool),
                "workflow disabled {mw} 后工具 {tool} 仍存在: {names:?}"
            );
        }
    }

    // builtin 实例关闭：workflow agent 的工具列表同样归零（IF-D10 面③）。
    for (policy_key, gone, survivor) in [
        (
            "WebMiddleware",
            "mcp__web__WebSearch",
            "mcp__artifact__artifact",
        ),
        (
            "ArtifactMiddleware",
            "mcp__artifact__artifact",
            "mcp__web__WebSearch",
        ),
    ] {
        let disabled: std::collections::HashSet<String> =
            std::iter::once(policy_key.to_string()).collect();
        let tools = pool_factory.build_tools("/tmp/contract-test", &disabled, None);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            !names.contains(&gone),
            "workflow disabled {policy_key} 后 {gone} 仍存在: {names:?}"
        );
        assert!(
            names.contains(&survivor),
            "workflow disabled {policy_key} 不得影响 {survivor}: {names:?}"
        );
    }

    // 两个实例都关闭：workflow 面 builtin 工具一个不剩。
    let both: std::collections::HashSet<String> = ["WebMiddleware", "ArtifactMiddleware"]
        .iter()
        .map(|name| name.to_string())
        .collect();
    let tools = pool_factory.build_tools("/tmp/contract-test", &both, None);
    assert!(
        !tools.iter().any(|tool| tool.name().starts_with("mcp__")),
        "两个 builtin 实例都关闭后 workflow 面不得残留 MCP 工具: {:?}",
        tools.iter().map(|t| t.name()).collect::<Vec<_>>()
    );
}

/// Workflow agent 中间件链按 disabled 集合过滤，未禁用项保持原相对顺序。
#[test]
fn workflow_build_middlewares_filters_disabled() {
    let factory = default_workflow_middleware_factory();
    // 全开：完整链序（与迁移前一致，顺序是行为契约）
    let all = factory.build_middlewares(
        &workflow_context_with_disabled(&[]),
        "contract-model",
        &["test-skill".to_string()],
        None,
    );
    let all_names: Vec<&str> = all.iter().map(|m| m.name()).collect();
    assert_eq!(
        all_names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "FilesystemMiddleware",
            "GitAttributionMiddleware",
            "TerminalMiddleware",
            "TodoMiddleware",
            "PermissionMiddleware",
        ]
    );

    // 逐个禁用：链上消失；剩余项相对顺序不变
    for mw in [
        "AgentsMdMiddleware",
        "SkillsMiddleware",
        "SkillPreloadMiddleware",
        "FilesystemMiddleware",
        "GitAttributionMiddleware",
        "TerminalMiddleware",
        "TodoMiddleware",
        "PermissionMiddleware",
    ] {
        let middlewares = factory.build_middlewares(
            &workflow_context_with_disabled(&[mw]),
            "contract-model",
            &["test-skill".to_string()],
            None,
        );
        let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
        assert!(
            !names.contains(&mw),
            "workflow disabled {mw} 后仍出现在链上: {names:?}"
        );
        // 未禁用项保持原顺序
        let baseline: Vec<&str> = all_names.iter().copied().filter(|n| *n != mw).collect();
        assert_eq!(names, baseline, "disabled {mw} 后剩余顺序漂移");
    }
}

#[tokio::test]
async fn workflow_shell_tools_reject_execution_after_session_owner_closes() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().to_str().unwrap();
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> = Arc::new(TaskManager::new());
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    let factory = default_workflow_middleware_factory();
    let mut tools = factory.build_tools(
        cwd,
        &std::collections::HashSet::new(),
        Some(manager.clone()),
    );
    for middleware in factory.build_middlewares(
        &workflow_context_with_disabled(&[]),
        "model",
        &[],
        Some(manager),
    ) {
        tools.extend(middleware.collect_tools(cwd));
    }
    let mut shell_count = 0;
    for tool in tools.into_iter().filter(|tool| tool.name() == "Bash") {
        shell_count += 1;
        let error = tool
            .invoke(
                serde_json::json!({"command": "echo leaked > unexpected"}),
                peri_agent::tools::ToolContext::new(&[], cwd),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("closing"), "{error}");
    }
    assert_eq!(shell_count, 2);
    assert!(!fixture.path().join("unexpected").exists());
}

// ─── 波 4 演进 C2/C3：段落持有者（基础段 + gated 段）────────────────────

use crate::default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
use crate::hitl::HumanInTheLoopMiddleware;
use crate::permission::PermissionMiddleware;
use crate::skills::SkillsMiddleware;
use crate::subagent::SubAgentMiddleware;

/// C2/C3 契约 3：链收集与渲染面静态声明一致（单一事实源，禁止双轨）——
/// 链上各段落持有者收集的段落（ID + 内容）与同一输入下的静态段声明
/// 逐项一致（C3：gated 段 10/11/13 持有者并入）。
#[test]
fn c2_chain_collection_matches_static_declaration() {
    let mut ctx = base_context();
    let overrides = AgentOverrides {
        persona: Some("chain persona".into()),
        tone: Some("chain tone".into()),
        proactiveness: None,
        mode: None,
    };
    ctx.agent_overrides = Some(overrides.clone());
    ctx.language = Some("zh-CN".to_string());

    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let collected = out.chain.collect_prompt_sections();

    let expected = DefaultSystemPromptMiddleware::sections(Some(&overrides))
        .into_iter()
        .chain(LangMiddleware::sections(Some("zh-CN")))
        .chain(PermissionMiddleware::sections())
        .chain(HumanInTheLoopMiddleware::sections())
        .chain(SubAgentMiddleware::sections())
        .chain(SkillsMiddleware::sections())
        .collect::<Vec<_>>();

    assert_eq!(collected.len(), expected.len(), "链收集段数与静态声明一致");
    for expect in &expected {
        let actual = collected
            .iter()
            .find(|s| s.id == expect.id)
            .unwrap_or_else(|| panic!("链收集缺少段落 {}", expect.id));
        assert_eq!(actual.content.as_str(), expect.content.as_str());
        assert_eq!(actual.zone, expect.zone);
        assert_eq!(actual.order, expect.order);
    }
    // persona / language 动态内容按同一输入生成（内容一致 = 禁止双轨）
    assert!(
        collected
            .iter()
            .find(|s| s.id == "persona")
            .unwrap()
            .content
            .as_str()
            .contains("chain persona"),
        "链收集 persona 内容与渲染面一致"
    );
}

/// C2 契约 3：关闭 DefaultSystemPromptMiddleware / LangMiddleware → 链上
/// 无持有者、收集结果无对应段落（基础段 + persona / language 全部消失）。
#[test]
fn c2_disable_holders_removes_sections_from_chain() {
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("DefaultSystemPromptMiddleware".to_string());
    ctx.meta_harness_disabled
        .insert("LangMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);

    let names: Vec<&str> = out.chain.names();
    assert!(
        !names.contains(&"DefaultSystemPromptMiddleware"),
        "关闭后 DefaultSystemPromptMiddleware 不应在链上: {names:?}"
    );
    assert!(
        !names.contains(&"LangMiddleware"),
        "关闭后 LangMiddleware 不应在链上: {names:?}"
    );
    let collected_ids: Vec<&str> = out
        .chain
        .collect_prompt_sections()
        .iter()
        .map(|s| s.id)
        .collect();
    assert!(
        !collected_ids.contains(&"01_intro")
            && !collected_ids.contains(&"07_runtime")
            && !collected_ids.contains(&"persona")
            && !collected_ids.contains(&"language"),
        "关闭持有者后其段落不应被收集: {collected_ids:?}"
    );
}

/// 盲区闭合（任务 4，契约 3）：关闭 gated 段持有者 → 段落与工具同时消失。
/// - 关闭 PermissionMiddleware → 10_hitl 段落消失（审批无 collect_tools 工具，
///   仅验证段落）；
/// - 关闭 HumanInTheLoopMiddleware → 12_ask_user 段落 + AskUserQuestion 工具
///   同时消失（2026-08-15 拆分后提问通道纳入关闭面）；
/// - 关闭 SubAgentMiddleware → 11_subagent 段落 + Agent/AgentResult 工具同时消失；
/// - 关闭 SkillsMiddleware → 13_skills 段落 + SkillTool/DiscoverSkillsTool 同时消失。
///
/// 段落关闭盲区（3.4 记载：关闭后段落仍渲染内置内容）随本批闭合。
#[test]
fn meta_harness_disabling_gated_holder_removes_section_and_tools() {
    // 基线：默认装配下四段全部收集
    let baseline_ctx = base_context();
    let baseline_ids: Vec<&str> = build_middleware_chain(&ProductionChainAssembler, &baseline_ctx)
        .chain
        .collect_prompt_sections()
        .iter()
        .map(|s| s.id)
        .collect();
    for id in ["10_hitl", "11_subagent", "12_ask_user", "13_skills"] {
        assert!(
            baseline_ids.contains(&id),
            "基线装配应收集 {id}: {baseline_ids:?}"
        );
    }

    let cases: &[(&str, &str, &[&str])] = &[
        (
            "PermissionMiddleware",
            "10_hitl",
            &[], // 审批无 collect_tools 工具
        ),
        (
            "HumanInTheLoopMiddleware",
            "12_ask_user",
            &["AskUserQuestion"],
        ),
        (
            "SubAgentMiddleware",
            "11_subagent",
            &["Agent", "AgentResult"],
        ),
        (
            "SkillsMiddleware",
            "13_skills",
            &["SkillTool", "DiscoverSkillsTool"],
        ),
    ];
    for (mw, section_id, gone_tools) in cases {
        let mut ctx = base_context();
        ctx.meta_harness_disabled.insert(mw.to_string());
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let collected_ids: Vec<&str> = out
            .chain
            .collect_prompt_sections()
            .iter()
            .map(|s| s.id)
            .collect();
        assert!(
            !collected_ids.contains(section_id),
            "关闭 {mw} 后段落 {section_id} 不应被收集: {collected_ids:?}"
        );
        let tool_names: Vec<String> = out
            .chain
            .collect_tools(&ctx.cwd)
            .into_iter()
            .map(|t| t.name().to_string())
            .collect();
        for tool in *gone_tools {
            assert!(
                !tool_names.iter().any(|n| n == tool),
                "关闭 {mw} 后工具 {tool} 仍可见: {tool_names:?}"
            );
        }
    }
}

/// 链收集与 `project_enabled_sections` 投影一致性（契约 3 显式视图）：
/// 链上收集到的 gated 段落 ID 集合 == 映射表投影（持有者在链上 → 段落开启）。
#[test]
fn chain_collected_gated_sections_match_projection() {
    use peri_agent::middleware::project_enabled_sections;
    use std::collections::HashSet;

    // 默认装配：持有者全部在链 → 投影包含全部三个 gated 段
    let ctx = base_context();
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let names: HashSet<&str> = out.chain.names().into_iter().collect();
    let projected = project_enabled_sections(&names);
    for id in ["10_hitl", "11_subagent", "13_skills"] {
        assert!(
            projected.contains(id),
            "持有者装配时投影应开启 {id}: {projected:?}"
        );
    }
    // 关闭 SubAgentMiddleware → 投影与链收集同时失去 11_subagent
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SubAgentMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let names: HashSet<&str> = out.chain.names().into_iter().collect();
    let projected = project_enabled_sections(&names);
    assert!(
        !projected.contains("11_subagent"),
        "关闭 SubAgentMiddleware 后投影应关闭 11_subagent: {projected:?}"
    );
    let collected_ids: HashSet<&str> = out
        .chain
        .collect_prompt_sections()
        .iter()
        .map(|s| s.id)
        .collect();
    assert!(
        !collected_ids.contains("11_subagent"),
        "链收集与投影一致（11_subagent 消失）: {collected_ids:?}"
    );
}

struct ReminderGoalController(std::sync::atomic::AtomicUsize);

#[async_trait]
impl GoalController for ReminderGoalController {
    async fn create_goal(&self, _: String) -> Result<(), String> {
        Ok(())
    }
    async fn complete_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn block_goal(&self, _: String) -> Result<(), String> {
        Ok(())
    }
    async fn clear_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn increment_continuation(&self) -> Result<(), String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn snapshot(&self) -> GoalViewSnapshot {
        GoalViewSnapshot {
            objective: Some("核对后台交付".into()),
            status: Some(peri_agent::goal::GoalStatus::Active),
            ..Default::default()
        }
    }
}

/// [回归测试] fallback manager 曾仅注入工具，未注入完成提醒 probe 与子任务 host。
/// 经真实 stage builder 和生产 Todo/Goal 槽位装配，不手工设置 idle_should_wait。
#[tokio::test]
async fn test_stage_completion_reminders_share_assembled_task_manager() {
    use peri_acp_types::session::SessionInbox;
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};
    use peri_agent::{
        agent::{react::AgentOutput, stages::middleware_runner},
        session::{
            exec::{
                executor::FrozenSessionData,
                stage_builder::{build_stage_context, StageBuildInput},
            },
            factory::{ChainAssembly, MiddlewareChainAssembler},
            FrozenContext, MessageQueue,
        },
    };
    struct CaptureAssembler(parking_lot::Mutex<Option<Arc<TaskManager>>>);
    impl MiddlewareChainAssembler for CaptureAssembler {
        type Context = AssemblyContext;
        type Output = ChainAssembly;
        fn assemble(&self, _: &[ChainSlot], ctx: &AssemblyContext) -> ChainAssembly {
            *self.0.lock() = Some(ctx.task_manager.clone());
            // 仅保留目标槽位，避免其他中间件读取用户配置或建立外部连接。
            ProductionChainAssembler.assemble(
                &[
                    ChainSlot::Terminal,
                    ChainSlot::Todo,
                    ChainSlot::SubAgent,
                    ChainSlot::Goal,
                ],
                ctx,
            )
        }
    }
    for supplied in [true, false] {
        for kind in [BgTaskKind::Agent, BgTaskKind::Workflow, BgTaskKind::Shell] {
            let fixture = tempfile::tempdir().unwrap();
            let queue = MessageQueue::new();
            let input = StageBuildInput {
                cwd: fixture.path().to_string_lossy().into_owned(),
                session_id: "completion-assembly".into(),
                cancel: Default::default(),
                broker: Arc::new(FakeBroker),
                permission_mode: SharedPermissionMode::new(PermissionMode::Default),
                plugin_skill_roots: vec![],
                plugin_loaded: vec![],
                hook_groups: vec![],
                session_start_source: None,
                cron_scheduler: None,
                mcp_pool: None,
                dynamic_mcp: None,
                session_mcp_capability: None,
                dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
                channel_state: None,
                tool_search_index: Arc::new(ToolSearchIndex::new()),
                shared_tools: Arc::new(RwLock::new(BTreeMap::new())),
                lsp_servers: vec![],
                lsp_pool: None,
                workflow_executor: None,
                workflow_middleware: None,
                thread_store: None,
                thread_id: None,
                model_name: "test".into(),
                provider_name: "test".into(),
                context_window: 128_000,
                claude_md_excludes: vec![],
                language: None,
                compact_config: Default::default(),
                retry_events: Default::default(),
                primary_llm_factory: Arc::new(|| Arc::new(FakeModel)),
                auto_classifier_factory: Arc::new(|| {
                    Arc::new(tokio::sync::Mutex::new(Box::new(FakeModel)))
                }),
                llm_factory: Arc::new(|_| Box::new(FakeLlm)),
                provider_fp: "test".into(),
                render_system_prompt: Arc::new(|_, _| String::new()),
                system_builder: Arc::new(|_, _| String::new()),
                langfuse_bridge_factory: None,
                shared_queue: queue.clone(),
                idle_inbox: Some(Arc::new(SessionInbox::new(Arc::new(queue)))),
                idle_suspended_flag: None,
                launch_cron_bridge: None,
                launch_mcp_subscription: None,
                mcp_skill_registry: None,
                command_registry: None,
                tool_invocation_resolver: Arc::new(peri_agent::tools::DirectToolInvocationResolver),
                compact_pre_hook: None,
                compact_post_hook: None,
                meta_harness_disabled: Default::default(),
            };
            let assembler = CaptureAssembler(parking_lot::Mutex::new(None));
            let manager = supplied.then(|| Arc::new(TaskManager::new()));
            let controller = Arc::new(ReminderGoalController(Default::default()));
            let (built, _) = build_stage_context(
                &input,
                &assembler,
                None,
                FrozenSessionData::from_frozen_parts(FrozenContext::builder().build(), None),
                Arc::new(FakeEventHandler),
                None,
                vec![],
                None,
                None,
                Default::default(),
                Some(controller.clone()),
                manager.clone(),
                None,
            )
            .unwrap();
            let assembled_manager = assembler.0.lock().clone().unwrap();
            if let Some(manager) = manager {
                assert!(
                    Arc::ptr_eq(&manager, &assembled_manager),
                    "不可替换注入的 session manager"
                );
            }
            let todo = built
                .context
                .runtime
                .tools
                .read()
                .get("TodoWrite")
                .unwrap()
                .clone();
            todo.invoke(
                serde_json::json!({
                    "requireCompletion": true,
                    "todos": [{"content": "等待后台交付", "status": "pending"}]
                }),
                peri_agent::tools::ToolContext::new(&[], &input.cwd),
            )
            .await
            .unwrap();
            let real_shell = cfg!(unix) && kind == BgTaskKind::Shell;
            if real_shell {
                // 实际执行装配出来的 Bash，验证工具端不是另一个 manager。
                let bash = built
                    .context
                    .runtime
                    .tools
                    .read()
                    .get("Bash")
                    .unwrap()
                    .clone();
                bash.invoke(
                    serde_json::json!({
                        "command": "while [ ! -f release ]; do sleep 0.01; done",
                        "run_in_background": true,
                        "timeout": 5000
                    }),
                    peri_agent::tools::ToolContext::new(&[], &input.cwd),
                )
                .await
                .unwrap();
                assert_eq!(assembled_manager.active_count(), 1);
            } else {
                assembled_manager
                    .register(BgTaskRegistration {
                        task_id: "pending".into(),
                        kind,
                        summary: "后台替身".into(),
                        pid: None,
                        kill: Some(Box::new(|| {})),
                    })
                    .unwrap();
            }
            let output = AgentOutput::new("等待后台完成", 1);
            let result = middleware_runner::run_after_agent(&built.context, output.clone())
                .await
                .unwrap();
            assert!(
                result.block_continue.is_none(),
                "supplied={supplied}, {kind:?}: 装配后的后台任务必须抑制提醒"
            );
            assert!(built.session.queue().is_empty());
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            let host = built
                .session
                .subagent_host()
                .expect("主 session 应有子任务 host");
            assert!(Arc::ptr_eq(
                host.task_manager.as_ref().unwrap(),
                &assembled_manager
            ));
            if real_shell {
                std::fs::write(fixture.path().join("release"), "go").unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while assembled_manager.active_count() > 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("Bash 应自然结束并移除活跃条目");
                assert_eq!(
                    assembled_manager.shutdown().await,
                    peri_acp_types::tasks::TaskShutdownReport::Complete
                );
            } else {
                assert!(assembled_manager.complete(
                    "pending",
                    peri_agent::agent::events::BackgroundTaskResult {
                        task_id: "pending".into(),
                        agent_name: "test".into(),
                        prompt_summary: String::new(),
                        success: true,
                        output: String::new(),
                        tool_calls_count: 0,
                        duration_ms: 0,
                        child_thread_id: None,
                        timed_out: false,
                        subagent_failure: None,
                        shell_output: None,
                    }
                ));
            }
            let result = middleware_runner::run_after_agent(&built.context, output.clone())
                .await
                .unwrap();
            assert_eq!(
                result.block_continue.as_deref(),
                Some("todo_require_completion")
            );
            assert_eq!(
                built.session.queue().drain_all().len(),
                1,
                "双开时 Todo 优先，不重复注入 Goal"
            );
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            todo.invoke(
                serde_json::json!({
                    "requireCompletion": true,
                    "todos": [{"content": "等待后台交付", "status": "completed"}]
                }),
                peri_agent::tools::ToolContext::new(&[], &input.cwd),
            )
            .await
            .unwrap();
            let result = middleware_runner::run_after_agent(&built.context, output.clone())
                .await
                .unwrap();
            assert_eq!(result.block_continue.as_deref(), Some("goal_active"));
            assert_eq!(built.session.queue().drain_all().len(), 1);
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 1);
            let mut blocked = output;
            blocked.block_continue = Some("stop_hook_block".into());
            let result = middleware_runner::run_after_agent(&built.context, blocked)
                .await
                .unwrap();
            assert_eq!(result.block_continue.as_deref(), Some("stop_hook_block"));
            assert!(built.session.queue().is_empty());
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }
}
