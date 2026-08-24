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
    Model, ModelCapabilities, ModelRequest, ModelResult, ModelStream, ModelStreamEvent,
};
use peri_resources::lsp::config::{LspConfigSource, LspServerConfig};
use peri_resources::workflow::protocol::{AgentRunParams, AgentRunResult};
use peri_resources::workflow::runner::AgentExecutor;

use crate::{
    agent_define::AgentOverrides,
    assembly::{
        create_session_lsp_pool, default_workflow_middleware_factory, load_merged_lsp_servers,
        AssemblyContext, OnBgCompleteFn, ProductionChainAssembler, SystemPromptBuilder,
    },
    hooks::{HookEvent, HookType, RegisteredHook},
    mcp::McpClientPool,
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

/// 蓝本槽位顺序 = 行为契约（7 组 21 槽，禁止重排；波 4 演进 C2 新增
/// DefaultSystemPrompt / Lang 于第一组首位——渲染排序不依赖链序，契约 2）。
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
            // 第二组：文件/终端/Web 工具提供器
            "Filesystem",
            "GitAttribution",
            "Terminal",
            "Web",
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
            "Artifact",
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
        ChainSlot::Terminal => "Terminal",
        ChainSlot::Web => "Web",
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
        ChainSlot::Artifact => "Artifact",
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
            "TerminalMiddleware",
            "WebMiddleware",
            "TodoMiddleware",
            "CronMiddleware",
            "PermissionMiddleware",
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "PtcMiddleware",
            "ToolSearch",
            "ArtifactMiddleware",
        ]
    );
}

/// Artifact 默认启用；单独关闭后仅移除 artifact，不影响 ToolSearch 元工具。
#[test]
fn artifact_middleware_can_be_disabled_independently() {
    let enabled = base_context();
    let enabled_tools = assemble_tool_names(&enabled);
    for expected in ["artifact", "SearchExtraTools", "ExecuteExtraTool"] {
        assert!(enabled_tools.iter().any(|name| name == expected));
    }

    let mut disabled = base_context();
    disabled
        .meta_harness_disabled
        .insert("ArtifactMiddleware".to_string());
    let disabled_tools = assemble_tool_names(&disabled);
    assert!(!disabled_tools.iter().any(|name| name == "artifact"));
    for expected in ["SearchExtraTools", "ExecuteExtraTool"] {
        assert!(disabled_tools.iter().any(|name| name == expected));
    }
    assert!(!assemble_names(&disabled)
        .iter()
        .any(|name| name == "ArtifactMiddleware"));
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
            "TerminalMiddleware",
            "WebMiddleware",
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
            "ArtifactMiddleware",
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
#[test]
fn meta_harness_disabled_tools_removed_from_chain() {
    let cases: &[(&str, &[&str])] = &[
        (
            "FilesystemMiddleware",
            &["Read", "Write", "Edit", "Glob", "Grep", "folder_operations"],
        ),
        ("TerminalMiddleware", &["Bash"]),
        ("WebMiddleware", &["WebFetch", "WebSearch"]),
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
/// 关闭 Filesystem/Terminal/Web 后 SubAgent 继承工具中无对应工具。
#[test]
fn meta_harness_disabled_parent_tools_filtered() {
    use crate::subagent::SubAgentMiddleware;

    let cases: &[(&str, &[&str])] = &[
        (
            "FilesystemMiddleware",
            &["Read", "Write", "Edit", "Glob", "Grep", "folder_operations"],
        ),
        ("TerminalMiddleware", &["Bash"]),
        ("WebMiddleware", &["WebFetch", "WebSearch"]),
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

/// `MIDDLEWARE_NAMES` 常量与 production_blueprint 的 21 个槽位 name 一一对应
/// （常量集合漂移即装配面缺失/多余条目）。
#[test]
fn middleware_names_match_production_blueprint() {
    let blueprint = production_blueprint();
    let slot_names: std::collections::HashSet<&str> =
        blueprint.iter().map(slot_middleware_name).collect();
    let const_names: std::collections::HashSet<&str> = MIDDLEWARE_NAMES.iter().copied().collect();
    assert_eq!(
        slot_names, const_names,
        "MIDDLEWARE_NAMES 必须与 production_blueprint 槽位完全一致"
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
        ChainSlot::Terminal => "TerminalMiddleware",
        ChainSlot::Web => "WebMiddleware",
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
        ChainSlot::Artifact => "ArtifactMiddleware",
        ChainSlot::Lsp => "LspMiddleware",
        ChainSlot::Goal => "GoalMiddleware",
    }
}

/// `MIDDLEWARE_TOOL_NAMES` 常量与各 middleware 静态工具名并集一致
/// （工具名漂移即防御剔除面失真；新增 middleware 工具须同步两处）。
#[test]
fn middleware_tool_names_match_static_tool_sets() {
    use crate::middleware::{FilesystemMiddleware, TerminalMiddleware};

    let static_tools: std::collections::HashSet<&str> = FilesystemMiddleware::tool_names()
        .into_iter()
        .chain(TerminalMiddleware::tool_names())
        .chain(HumanInTheLoopMiddleware::tool_names())
        .chain([
            // WebMiddleware
            "WebFetch",
            "WebSearch",
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
            // ArtifactMiddleware
            "artifact",
            // LspMiddleware
            "LSP",
            // GoalMiddleware
            "goal",
            // McpMiddleware（静态部分）
            "DiscoverMCP",
            "mcp_read_resource",
        ])
        .collect();
    let const_tools: std::collections::HashSet<&str> =
        MIDDLEWARE_TOOL_NAMES.iter().copied().collect();
    assert_eq!(
        static_tools, const_tools,
        "MIDDLEWARE_TOOL_NAMES 必须与各 middleware 静态工具名并集一致"
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
        Arc::new(|_, _, _| {});
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
#[test]
fn workflow_build_tools_filters_disabled() {
    let factory = default_workflow_middleware_factory();
    // 全开：fs + terminal + web + skills 工具齐全
    let all = factory.build_tools("/tmp/contract-test", &std::collections::HashSet::new());
    let all_names: Vec<&str> = all.iter().map(|t| t.name()).collect();
    for expected in [
        "Read",
        "Bash",
        "WebFetch",
        "WebSearch",
        "SkillTool",
        "DiscoverSkillsTool",
    ] {
        assert!(
            all_names.contains(&expected),
            "全开时工具 {expected} 应存在: {all_names:?}"
        );
    }

    let cases: &[(&str, &[&str])] = &[
        (
            "FilesystemMiddleware",
            &["Read", "Write", "Edit", "Glob", "Grep"],
        ),
        ("TerminalMiddleware", &["Bash"]),
        ("WebMiddleware", &["WebFetch", "WebSearch"]),
        ("SkillsMiddleware", &["SkillTool", "DiscoverSkillsTool"]),
    ];
    for (mw, expected_gone) in cases {
        let disabled: std::collections::HashSet<String> = std::iter::once(mw.to_string()).collect();
        let tools = factory.build_tools("/tmp/contract-test", &disabled);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for tool in *expected_gone {
            assert!(
                !names.contains(tool),
                "workflow disabled {mw} 后工具 {tool} 仍存在: {names:?}"
            );
        }
    }
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
            "WebMiddleware",
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
        "WebMiddleware",
        "TodoMiddleware",
        "PermissionMiddleware",
    ] {
        let middlewares = factory.build_middlewares(
            &workflow_context_with_disabled(&[mw]),
            "contract-model",
            &["test-skill".to_string()],
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
