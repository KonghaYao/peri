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
use peri_model::{Model, ModelCapabilities, ModelRequest, ModelResult, ModelStream};
use peri_resources::lsp::config::LspServerConfig;
use peri_resources::workflow::protocol::{AgentRunParams, AgentRunResult};
use peri_resources::workflow::runner::AgentExecutor;

use crate::{
    agent_define::AgentOverrides,
    assembly::{AssemblyContext, OnBgCompleteFn, ProductionChainAssembler, SystemPromptBuilder},
    hitl::{PermissionMode, SharedPermissionMode},
    hooks::{HookEvent, HookType, RegisteredHook},
    mcp::McpClientPool,
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
        cron_scheduler: None,
        mcp_pool: None,
        channel_state: None,
        tool_search_index: Arc::new(ToolSearchIndex::new()),
        shared_tools,
        lsp_servers: Vec::new(),
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

/// 蓝本槽位顺序 = 行为契约（7 组 19 槽，禁止重排）。
#[test]
fn blueprint_sequence_is_canonical() {
    let slots = production_blueprint();
    let names: Vec<&str> = slots.iter().map(|s| slot_name(s)).collect();
    assert_eq!(
        names,
        vec![
            // 第一组：上下文注入器
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
            // 第五组：HITL + SubAgent
            "Hitl",
            "SubAgent",
            // 第六组：MCP / Workflow / ToolSearch
            "Mcp",
            "Workflow",
            "ToolSearch",
            // 第七组：LSP / Goal（Goal 在链最后）
            "Lsp",
            "Goal",
        ]
    );
}

fn slot_name(slot: &ChainSlot) -> &'static str {
    match slot {
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
        ChainSlot::Hitl => "Hitl",
        ChainSlot::SubAgent => "SubAgent",
        ChainSlot::Mcp => "Mcp",
        ChainSlot::Workflow => "Workflow",
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
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "ToolSearch",
        ]
    );
}

/// 权限模式不影响链组成与 HITL 位置（四种模式一致）。
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
            Some(13),
            "mode {mode:?}: HITL 位置漂移"
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
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "McpMiddleware",
            "WorkflowMiddleware",
            "ToolSearch",
            "LspMiddleware",
            "GoalMiddleware",
        ]
    );
}
