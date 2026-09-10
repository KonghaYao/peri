//! Shared Agent builder（L5：自 peri-acp/src/host/exec/stage_builder.rs 迁入；
//! 原 `agent::builder` 全路径引用改 crate::，ACP 特有构造经注入参数接入）。
//!
//! 提供 `build_agent()` 构建函数：构造装配上下文，经 Agent 层 session 工厂
//! 构建中间件链，并产出 `AgentComponents`（供 v2 builder 消费）。
//! 链装配实现已随 L2 迁出（装配上下文 `factory::AssemblyContext` 同层，
//! 装配器经 `factory::MiddlewareChainAssembler` trait 注入——ACP 装配点
//! 传 `ProductionChainAssembler`，本模块不触碰 middlewares 实现）。
//!
//! 依赖反转（§0）：本模块只依赖 peri-acp-types / peri-model / crate 内部；
//! LLM 构造（LlmProvider / AgentPool / RetryObserver 烘焙）、system prompt
//! 渲染、Langfuse bridge、compact hooks 与 tool resolver 全部经
//! [`StageBuildInput`] 注入面接入。

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use parking_lot::RwLock;
use peri_acp_types::{
    agents::AgentOverrides,
    command_registry::CommandRegistry,
    compact::CompactConfig,
    cron::CronSchedulerPort,
    event::{AgentEventHandler, ExecutorEvent},
    event_v2::{EventBus, EventBusConfig, EventHandles},
    frozen::{ChildHandlerFactory, ThreadPersistence},
    goal::GoalController,
    hooks::RegisteredHook,
    identity::AgentId,
    interaction::{ChannelState, UserInteractionBroker},
    lsp::LspServerConfig,
    mcp_skills::McpSkillRegistry,
    plugin::LoadedPlugin,
    ports::{
        LspPoolPort, McpPoolPort, SessionMcpCapabilityPort, ToolSearchPort, WorkflowMiddlewarePort,
    },
    session::{MessageQueue, SessionInbox},
    skills::SkillRoot,
    store::ThreadStore,
    tools::TodoItem,
    workflow::AgentExecutor,
};

use crate::agent::{
    async_tasks::TaskManager,
    react::ReactLLM,
    stages::{SharedToolMap, StageContext},
    token::ContextBudget,
    LangfuseBridgeLike,
};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::middleware::chain::MiddlewareChain;
use crate::session::exec::executor::FrozenSessionData;
use crate::session::factory::{
    AssemblyContext, ChainAssembly, MiddlewareChainAssembler, OnBgCompleteFn,
    SubAgentMiddlewarePort, SystemPromptBuilder,
};
use crate::session::retry_events::RetryEventForwarder;
use crate::session::Session;
use crate::tools::{BaseTool, ToolInvocationResolver};

mod agent;
mod dependencies;
mod session_setup;
mod subagent_setup;
mod tools;

pub(crate) use agent::build_agent;
use tools::build_session_tool_view;

// ── 装配/构建输入（原 SessionContext 投影 + 注入面）──────────────────────────

/// stage 装配输入（L5：ACP 装配点从 `SessionContext` 投影构造并经注入面补齐）。
///
/// 字段分两组：
/// - 会话数据：原 `SessionContext` 的契约化投影（会话级共享值）；
/// - 注入面：原 ACP 特有构造（LLM provider / 渲染 / 观测 / 装配器依赖）
///   的参数化入口——ACP 侧保留实现，本模块只消费。
#[allow(clippy::type_complexity)]
pub struct StageBuildInput {
    // ── 会话数据 ──
    /// 工作目录
    pub cwd: String,
    /// 会话 ID（主 agent LLM session_id 注入 + 事件身份）
    pub session_id: String,
    /// 取消令牌（Session 共享）
    pub cancel: tokio_util::sync::CancellationToken,
    /// 用户交互 broker（HITL 审批）
    pub broker: Arc<dyn UserInteractionBroker>,
    /// 权限模式（SharedPermissionMode）
    pub permission_mode: Arc<peri_acp_types::permission::SharedPermissionMode>,
    /// 插件技能根目录
    pub plugin_skill_roots: Vec<SkillRoot>,
    /// 已加载插件
    pub plugin_loaded: Vec<LoadedPlugin>,
    /// Hook 组（每组一个 HookMiddleware 实例）
    pub hook_groups: Vec<Vec<RegisteredHook>>,
    /// session 启动来源（hook 注入用）
    pub session_start_source: Option<String>,
    /// Cron 调度器端口（print 模式 turn 级 CronOwner 用）
    pub cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    /// MCP 连接池端口
    pub mcp_pool: Option<Arc<dyn McpPoolPort>>,
    /// Deployment-scoped Dynamic MCP operation port.
    pub dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
    /// Session-scoped Dynamic MCP capability source.
    pub session_mcp_capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
    /// Session-owned checked projection lease holder, shared across stage builds.
    pub dynamic_mcp_projection:
        Arc<parking_lot::Mutex<Option<Arc<dyn peri_acp_types::ports::SessionMcpProjectionLease>>>>,
    /// Channel 状态
    pub channel_state: Option<Arc<ChannelState>>,
    /// 工具搜索索引端口
    pub tool_search_index: Arc<dyn ToolSearchPort>,
    /// 共享工具注册表（deferred tools）
    pub shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    /// LSP 服务器配置
    pub lsp_servers: Vec<LspServerConfig>,
    /// 会话级 LSP 服务器池端口（复用，None = 构造临时实例）
    pub lsp_pool: Option<Arc<dyn LspPoolPort>>,
    /// Workflow executor（Some 时注册 Workflow 中间件）
    pub workflow_executor: Option<Arc<dyn AgentExecutor>>,
    /// 会话级 WorkflowMiddleware 端口
    pub workflow_middleware: Option<Arc<dyn WorkflowMiddlewarePort>>,
    /// 持久化存储（transcript persistence 激活）
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 当前会话 thread ID
    pub thread_id: Option<String>,
    // ── 注入面（原 ACP 特有构造）──
    /// 模型名称（GitAttribution / hook 注入用）
    pub model_name: String,
    /// 模型显示名（hook / Langfuse bridge 用）
    pub provider_name: String,
    /// 上下文窗口（已含 context_1m 调整；token 监控）
    pub context_window: u32,
    /// CLAUDE.md 排除项
    pub claude_md_excludes: Vec<String>,
    /// 会话语言（frozen，sub prompt 渲染用）
    pub language: Option<String>,
    /// Compact 配置（ACP 装配点按 `load_compact_config` 语义预填，含 env overrides）
    pub compact_config: CompactConfig,
    /// Session 级 retry 事件转发器（池化模型烘焙的 observer 同源；
    /// 每 turn 覆盖式 set 当前 handler）
    pub retry_events: RetryEventForwarder,
    /// 主 LLM 构造工厂（ACP 侧完成 fingerprint / AgentPool 缓存 / RetryObserver 烘焙）
    pub primary_llm_factory: Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>,
    /// auto-classifier 模型构造工厂（cached 缺失时调用）
    pub auto_classifier_factory:
        Arc<dyn Fn() -> Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>> + Send + Sync>,
    /// 子 agent LLM 工厂（支持 SubAgent LLM 缓存复用）
    pub llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    /// provider fingerprint（CachedLlmInstances 缓存键）
    pub provider_fp: String,
    /// agent overrides 渲染（主 prompt 覆盖；含 workflow feature 判定）
    pub render_system_prompt: Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>,
    /// SubAgent system prompt 构建器（无 workflow feature + frozen date）
    pub system_builder: SystemPromptBuilder,
    /// SubAgent Langfuse bridge 工厂（采样决策继承自父 agent）
    pub langfuse_bridge_factory: Option<Arc<dyn Fn() -> Arc<dyn LangfuseBridgeLike> + Send + Sync>>,
    /// 会话级共享 v2 MessageQueue（每 turn 同一实例，跨 turn 存活）
    pub shared_queue: MessageQueue,
    /// 会话级 SessionInbox（allow_await_wake 路径；ACP 装配点判断）
    pub idle_inbox: Option<Arc<SessionInbox>>,
    /// 会话级 idle-suspended 标志（await_wake 挂起期间置 true；宿主
    /// dispatch_prompt_turn 据此把挂起期间到达的用户 prompt 注入 inbox）。
    pub idle_suspended_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// session 级 cron bridge 惰性启动器（SessionManager 路径；无则走
    /// print 模式 turn 级 CronOwner）
    pub launch_cron_bridge: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    /// session 级 MCP 订阅 inbox 惰性注册器（SessionManager 路径；无则
    /// 安全 no-op——订阅通知不唤醒 print 模式单次进程）
    pub launch_mcp_subscription: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    /// 会话级 MCP skill 远端注册表（SessionAccessPort 投影；None = print
    /// 模式，跳过发现与合并）。
    pub mcp_skill_registry: Option<Arc<McpSkillRegistry>>,
    /// 会话级命令注册表（命令面，Phase 6 A3；SessionAccessPort 投影；
    /// None = print 模式，跳过 mcp 域命令发现投影）。
    pub command_registry: Option<Arc<CommandRegistry>>,
    /// tool invocation resolver（wrapper-aware canonical resolver）
    pub tool_invocation_resolver: Arc<dyn ToolInvocationResolver>,
    /// compact 前置 hook（hook_groups 非空时 ACP 装配点构造）
    pub compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// compact 后置 hook（hook_groups 非空时 ACP 装配点构造）
    pub compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
    /// 装配期关闭的 middleware 名集合（源自当前 `FrozenSessionData` 的
    /// `v2_frozen.meta_harness.disabled_middlewares` 投影；
    /// 顶层链过滤——设计 §2.5）。
    pub meta_harness_disabled: HashSet<String>,
}

/// 后台任务完成事件的独立发送端（跨 turn 存活；L3：注入 SubagentHost）
pub type BgEventTx = tokio::sync::mpsc::UnboundedSender<ExecutorEvent>;

/// Session-scoped cached LLM instances（L5：自 ACP `session::agent_pool` 迁入，
/// ACP 保留 re-export）。
///
/// Contains `reqwest::Client` with connection pool + TLS session cache.
/// Reusing across prompts eliminates transient per-turn allocations.
#[derive(Clone)]
pub struct CachedLlmInstances {
    /// 辅助 LLM（v2 stages/compact.rs 摘要 + Goal 工具验证共用）。
    pub auxiliary_model: Arc<dyn peri_model::Model>,
    /// auto_classifier LLM (used by HITL HumanInTheLoopMiddleware).
    pub auto_classifier_model: Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>>,
    /// Provider fingerprint at time of creation (`"provider_name:model_name:think=effort:budget"`).
    pub fingerprint: String,
}

// ── 共享 Agent 构建（ACP 和 TUI 共用）─────────────────────────────────────────
//
// 链装配（含 SubAgentMiddleware 构造点）已随 L2 迁出：
// - 唯一触发点与链序事实源：`crate::session::factory::build_middleware_chain`
//   + `production_blueprint`（ARC-MIDDLEWARE-001）
// - 装配实现：`peri-middlewares::assembly::ProductionChainAssembler`
//   （经 [`MiddlewareChainAssembler`] trait 注入，本模块不引用实现）
// - 装配上下文：`crate::session::factory::AssemblyContext`（L5 迁入本层）

pub(crate) struct AcpAgentOutput {
    pub components: AgentComponents,
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<TodoItem>>,
    /// 后台任务完成事件的独立接收端（不随 executor 生命周期销毁）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
    /// 后台任务完成事件的发送端（L3：注入 SubagentHost，子 agent bg 事件经此
    /// 通道到达 executor_helpers 的 bg event pump）
    pub bg_event_tx: BgEventTx,
}

/// Agent 装配产物（v2 builder 直接消费，P5.3 抽取）
///
/// `build_agent` 经 Agent 层 session 工厂装配 `MiddlewareChain`，
/// 并组装 LLM + system prompt 等字段产出本结构，
/// `build_stage_context` 消费它构造 v2 `StageContext`。
pub struct AgentComponents {
    /// 主 LLM（已通过 `AgentModelBridge` 适配为标准 ReAct 抽象）
    pub llm: Arc<dyn ReactLLM + Send + Sync>,
    /// 中间件链（v2 StageContext 直接复用）
    pub chain: Arc<MiddlewareChain>,
    /// 共享工具注册表（deferred tools，供 ExecuteExtraTool 代理）
    #[allow(clippy::type_complexity)]
    pub shared_tools: Option<Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>>,
    /// 错误感知建议注册表
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（工具名 + subagent 类型）
    pub tool_registry_snapshot: Arc<ToolRegistrySnapshot>,
    /// 上下文预算（token 监控）
    pub context_budget: Option<ContextBudget>,
    /// Compact 配置
    pub compact_config: Option<CompactConfig>,
    /// SubAgent 中间件端口（chain 中已有一份 clone；本字段保留原实例，
    /// 供 build_stage_context 在主 v2 session 创建后注入 parent_agent_id）
    pub subagent_mw: Option<Arc<dyn SubAgentMiddlewarePort>>,
}

// ── v2 StageContext 构建（合并自 builder_v2.rs）────────────────────────────────
//
// 直接构造 StageContext 供 run_react_loop 消费。
// 复用上方 build_agent() 的中间件链与 LLM 构造（AgentComponents），避免重复 700+ 行装配逻辑。
//
// ## 工具注入
//
// run_react_loop 每轮从 shared_tools（SharedToolMap）按名读取工具，
// 不会每轮重新填充。因此 build_stage_context 内部显式调用
// chain.collect_tools(cwd) 把 middleware 提供的工具一次性 merge 到
// shared_tools（2026-08-15 拆分后宿主级 shared_tools 写入点归零，
// AskUserQuestion 由链上 HumanInTheLoopMiddleware 提供，不再单独
// register_tool；本地同名工具由当前链实例覆盖，宿主共享表不改写）。
//
// ## Async Owners
//
// 有 SessionManager 的路径（TUI/stdio）：cron bridge 由
// `SessionManager::cron_bridge_for` 在 AcpSession 上懒启动（session 级，
// 跨 turn 存活，见 spec/issues/2026-08-04-cron-trigger-lost-after-turn-error.md），
// 本函数不再挂载 turn 级 CronOwner——经注入的 `launch_cron_bridge` 触发。
//
// 仅 print 模式（-p，无 SessionManager）走本函数的 turn 级挂载：
// 1. 创建 SessionInbox（await-wake wrapper around shared_queue）。
// 2. 从 CronScheduler 端口订阅 trigger_rx。
// 3. 启动 CronTrigger→String 桥接任务。
// 4. 创建并启动 CronOwner（trigger_rx → inbox）。
// 5. 通过 Session::set_async_owners 注入到 Session。

/// v2 builder 产物
pub struct V2AgentOutput {
    /// 已配置的 StageContext（用于 run_react_loop）
    pub context: StageContext,
    /// v2 Session（持有 transcript + queue + store）
    pub session: Arc<Session>,
    /// EventBus 消费端（转 ExecutorEvent 用）
    pub event_handles: EventHandles,
    /// Todo 更新通道（spawn todo forwarder 用）
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<TodoItem>>,
    /// 后台任务完成事件接收端（spawn bg event pump 用）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
}

/// 从 [`StageBuildInput`] 构造 StageContext
///
/// 内部调用 build_agent 提取 middleware chain + LLM + 共享组件（AgentComponents），
/// 然后构造 StageContext。
///
/// **shared_queue**：会话级共享的 v2 MessageQueue。每个 turn 调用本函数时
/// 必须传入**同一个**实例（来自 AcpSession.v2_message_queue），让本 turn 的
/// StageContext.queue 与会话级共享。
///
/// MessageQueue 内部 Arc<Mutex<VecDeque>> + Arc<Notify>，clone 共享底层；
/// 传入引用只是为了避免在签名里 move。
#[derive(Debug, thiserror::Error)]
pub enum StageBuildError {
    #[error("session tool catalog is invalid: {0}")]
    ToolCatalog(#[from] crate::session::tool_catalog::CatalogRefreshError),
    #[error("dynamic MCP catalog registration failed: {0:?}")]
    DynamicMcp(peri_acp_types::dynamic_mcp::DynamicMcpFailure),
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn build_stage_context(
    input: &StageBuildInput,
    assembler: &dyn MiddlewareChainAssembler<Context = AssemblyContext, Output = ChainAssembly>,
    cached_llm: Option<&CachedLlmInstances>,
    frozen_session: FrozenSessionData,
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<ChildHandlerFactory>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    thread_persistence: ThreadPersistence,
    goal_controller: Option<Arc<dyn GoalController>>,
    task_manager: Option<Arc<TaskManager>>,
    on_bg_complete: Option<OnBgCompleteFn>,
) -> Result<(V2AgentOutput, Option<CachedLlmInstances>), StageBuildError> {
    // 提取 LLM 用字段（在 cfg 被 build_agent 消费前）
    let cwd = input.cwd.clone();
    let session_id = input.session_id.clone();
    let cancel_token = input.cancel.clone();
    // compact_llm：优先取 auxiliary_model，否则回落到 cached auxiliary_model。
    let compact_llm_for_v2 = auxiliary_model
        .clone()
        .or_else(|| cached_llm.map(|c| c.auxiliary_model.clone()));

    // 提取 cron_scheduler（端口直接 subscribe；无 SessionManager 的 print 路径）
    let cron_scheduler = input.cron_scheduler.clone();

    // 会话级共享变量（注入面）
    let shared_queue = input.shared_queue.clone();
    let idle_inbox = input.idle_inbox.clone();

    let idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>> = {
        let probe_bg = task_manager.clone();
        probe_bg.map(|reg| {
            Arc::new(move || reg.active_count() > 0) as Arc<dyn Fn() -> bool + Send + Sync>
        })
    };

    // 调用 build_agent 构造完整 agent（含中间件链 + LLM）
    // L3：build_agent 消费的字段先 clone 一份（host 注入需要在主 session
    // 创建后使用同一份数据）
    let (agent_output, new_cached) = build_agent(
        input,
        assembler,
        &frozen_session,
        event_handler,
        agent_overrides,
        preload_skills,
        child_handler_factory,
        auxiliary_model,
        thread_persistence.clone(),
        goal_controller.clone(),
        task_manager.clone(),
        on_bg_complete.clone(),
        cached_llm,
    );

    // 直接消费 AgentComponents
    let AgentComponents {
        llm,
        chain,
        shared_tools: shared_tools_opt,
        error_suggest_registry,
        tool_registry_snapshot,
        context_budget,
        compact_config,
        subagent_mw,
    } = agent_output.components;
    let bg_event_tx = agent_output.bg_event_tx;

    let shared_tools: SharedToolMap = shared_tools_opt
        .unwrap_or_else(|| Arc::new(RwLock::new(std::collections::BTreeMap::new())));

    let cancel_arc = Arc::new(cancel_token);
    let session = session_setup::build_session(
        input,
        &frozen_session,
        &cwd,
        &session_id,
        &cancel_arc,
        &shared_queue,
        &cron_scheduler,
    );

    let turn = session.start_turn();
    let transcript = session.transcript();
    let queue = session.queue().clone();

    // 创建 EventBus
    let (event_bus, event_handles) = EventBus::new(EventBusConfig::default());

    // session_context 键值
    let session_context = Arc::new(RwLock::new({
        let mut map = std::collections::HashMap::new();
        map.insert("session_id".to_string(), session_id.clone());
        map
    }));

    // 复用 build_agent 产出的 LLM（已适配为 ReactLLM）
    let react_llm = llm;

    // 主 agent 事件侧身份（C2）：StageContext agent_id 与 SubAgentTool 共享 cell
    // 必须同一值——subagent 补发的 SubagentStart.agent_id 指回主 agent。
    let main_agent_id = AgentId::new();

    subagent_setup::attach_subagent_host(
        input,
        &session,
        main_agent_id,
        &subagent_mw,
        subagent_setup::SubagentDependencies {
            frozen_session: &frozen_session,
            thread_persistence: &thread_persistence,
            task_manager: &task_manager,
            on_bg_complete: &on_bg_complete,
            bg_event_tx,
        },
    );

    // [时序契约] 工具注入必须晚于 parent_session 注入：SubAgentTool 在
    // build_tool（collect_tools）时读取 parent_session 以获取运行时 host
    // （task_manager / bg_event_sender / thread_store / frozen 回退）——先于
    // 注入则 host 为空，`run_in_background: true` 会静默降级为同步执行
    // （bg subagent 不注册 TaskManager，BgTaskArea 无运行条目，
    // issue 2026-08-06-e2e-bg-task-area-entry-missing）。每轮重建，顺序不可调换。
    // 当前链的有状态工具覆盖本地同名项；宿主级共享表保持不变。
    //
    // MetaHarness（设计 §2.5）：session/turn 级工具视图——基础 shared_tools
    // 是宿主级共享 registry（2026-08-15 拆分后生产路径写入点归零；
    // middleware 工具从不写入，只经本调用 merge 进每轮重建的本地视图）。
    // 从基础表复制时剔除"middleware 静态工具名且不在当前链工具集合"的
    // 条目（防御面，决策记录见 `MIDDLEWARE_TOOL_NAMES` 注释）——disabled
    // session 的本地视图不得看到残留的 middleware 工具。动态 MCP bridge
    // 工具（`mcp__{server}__{tool}`）不进入共享 registry，无需剔除。
    let session_tools: SharedToolMap =
        build_session_tool_view(&shared_tools, chain.collect_tools(&cwd));
    let tool_catalog = tools::register_tool_catalog(input, &session_tools)?;

    // 构造 StageContext（builder 构造晚于工具注入：chain 在
    // collect_tools 借用后被 move 进 builder，顺序不可调换）
    let builder = StageContext::builder(turn, transcript, queue)
        .with_agent_id(main_agent_id)
        .with_llm(react_llm)
        .with_tools(session_tools)
        .with_tool_catalog(tool_catalog)
        .with_tool_invocation_resolver(Arc::clone(&input.tool_invocation_resolver))
        .with_middleware_chain(Arc::clone(&chain))
        .with_event_bus(Arc::new(event_bus))
        .with_session_context(session_context)
        .with_tool_registry_snapshot((*tool_registry_snapshot).clone());

    let builder = dependencies::configure_stage(
        builder,
        input,
        &session,
        dependencies::StageDependencies {
            goal_controller,
            error_suggest_registry,
            context_budget,
            compact_config,
            compact_llm_for_v2,
            idle_inbox,
            idle_should_wait,
        },
    );

    let context = builder.build();

    Ok((
        V2AgentOutput {
            context,
            session,
            event_handles,
            todo_rx: agent_output.todo_rx,
            bg_event_rx: agent_output.bg_event_rx,
        },
        new_cached,
    ))
}

#[cfg(test)]
#[path = "stage_builder/builder_v2_test.rs"]
mod builder_v2_tests;
