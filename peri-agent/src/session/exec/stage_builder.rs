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

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::{
    agents::AgentOverrides,
    compact::CompactConfig,
    cron::CronSchedulerPort,
    event::{AgentEventHandler, ExecutorEvent},
    event_v2::{EventBus, EventBusConfig, EventHandles},
    frozen::{ChildHandlerFactory, FrozenData, ThreadPersistence},
    goal::GoalController,
    hooks::RegisteredHook,
    identity::AgentId,
    interaction::{ChannelState, UserInteractionBroker},
    lsp::LspServerConfig,
    plugin::LoadedPlugin,
    ports::{LspPoolPort, McpPoolPort, ToolSearchPort, WorkflowMiddlewarePort},
    session::{CronOwner, MessageQueue, SessionInbox},
    skills::SkillRoot,
    store::ThreadStore,
    tools::TodoItem,
    workflow::AgentExecutor,
};

use crate::agent::{
    async_tasks::TaskManager,
    model_bridge::AgentModelBridge,
    react::ReactLLM,
    stages::{SharedToolMap, StageContext},
    token::ContextBudget,
    LangfuseBridgeLike,
};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::middleware::chain::MiddlewareChain;
use crate::session::factory::{
    AssemblyContext, ChainAssembly, MiddlewareChainAssembler, OnBgCompleteFn,
    SubAgentMiddlewarePort, SystemPromptBuilder,
};
use crate::session::retry_events::RetryEventForwarder;
use crate::session::subagent::SubagentHost;
use crate::session::{FrozenContext, Session};
use crate::tools::{BaseTool, ToolInvocationResolver};

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
    /// tool invocation resolver（wrapper-aware canonical resolver）
    pub tool_invocation_resolver: Arc<dyn ToolInvocationResolver>,
    /// compact 前置 hook（hook_groups 非空时 ACP 装配点构造）
    pub compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// compact 后置 hook（hook_groups 非空时 ACP 装配点构造）
    pub compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
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
    pub chain: MiddlewareChain,
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

/// 构建可复用的 Agent（ACP 和 TUI 共用核心构建逻辑）
///
/// 中间件链装配经 Agent 层 session 工厂（链序蓝本 `production_blueprint`，
/// ARC-MIDDLEWARE-001）与注入的装配器完成，本函数构造装配上下文并组装
/// LLM/prompt/缓存。
///
/// `cached_llm` 允许跨 prompt 复用 LLM 实例（auxiliary_model、
/// auto_classifier_model），避免每轮重建 reqwest::Client（~1-2 MB/实例）。
/// 首次调用传 `None`，后续调用传上一次返回的 `Some(CachedLlmInstances)`。
#[allow(clippy::too_many_arguments)] // 过渡：AAC 字段已拆分为独立参数
pub(crate) fn build_agent(
    input: &StageBuildInput,
    assembler: &dyn MiddlewareChainAssembler<Context = AssemblyContext, Output = ChainAssembly>,
    system_prompt: String,
    subagent_system_prompt: Option<String>,
    frozen: FrozenData,
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<ChildHandlerFactory>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    thread_persistence: ThreadPersistence,
    goal_controller: Option<Arc<dyn GoalController>>,
    task_manager: Option<Arc<TaskManager>>,
    on_bg_complete: Option<OnBgCompleteFn>,
    cached_llm: Option<&CachedLlmInstances>,
) -> (AcpAgentOutput, Option<CachedLlmInstances>) {
    let FrozenData {
        claude_md: frozen_claude_md,
        claude_local_md: frozen_claude_local_md,
        skill_summary: frozen_skill_summary,
        date: _frozen_date,
    } = frozen;

    let ThreadPersistence {
        store: thread_store,
        parent_thread_id,
        register_runtime,
        deregister_runtime,
    } = thread_persistence;

    // 从 StageBuildInput 提取共享字段
    let cwd = input.cwd.clone();
    let cancel = input.cancel.clone();
    let permission_mode = input.permission_mode.clone();
    let cron_scheduler = input.cron_scheduler.clone();
    let session_id = Some(input.session_id.clone());
    let permission_broker = input.broker.clone();
    let plugin_skill_roots = input.plugin_skill_roots.clone();
    let plugin_loaded = input.plugin_loaded.clone();
    let hook_groups = input.hook_groups.clone();
    let session_start_source = input.session_start_source.clone();
    let mcp_pool = input.mcp_pool.clone();
    let channel_state = input.channel_state.clone();
    let tool_search_index = input.tool_search_index.clone();
    let shared_tools = input.shared_tools.clone();
    let lsp_servers = input.lsp_servers.clone();
    let workflow_executor = input.workflow_executor.clone();
    let workflow_middleware = input.workflow_middleware.clone();
    let mw_auxiliary_model = auxiliary_model;

    // Retry observer 转发器（session 级，挂 AgentPool）：本 turn 的 event_handler
    // 在构造模型前覆盖式 set，池化模型烘焙转发器引用，发射时读取当前 turn 的
    // 最新 handler——跨 turn 不陈旧。
    let retry_events = input.retry_events.clone();
    retry_events.set(Some(Arc::clone(&event_handler)));

    // Capture system_prompt before it may be overridden below (for SubAgent fork reuse).
    // [P2-2026-08-02] fork / subagent 复用的冻结 prompt 必须是"无 16_workflow"
    // 版本（`FrozenSessionData::subagent_system_prompt`）：fork 链不注册
    // WorkflowTool（shared_tools: None），继承带 workflow 声明的 parent frozen
    // prompt 会造成 prompt 与能力矛盾。调用方未提供时回退到主 prompt（防御）。
    let system_prompt_for_sub = subagent_system_prompt.unwrap_or_else(|| system_prompt.clone());

    // 应用 agent overrides 到系统提示词
    let system_prompt = agent_overrides.as_ref().map_or_else(
        || system_prompt.clone(),
        |ov| (input.render_system_prompt)(Some(ov), &cwd),
    );

    // 提前提取模型实例（chain 构建完成后才组装 AgentModelBridge，
    // 以便收集中间件 prompt_contribution 合并到 system prompt）。
    // 与 SubAgent 模型共享 session 级 LLM 缓存（同一 fingerprint）：
    // 跨 turn / 跨 agent 实例复用 reqwest::Client（连接池 + TLS session cache），
    // 避免每轮重建 ~1-2 MB HTTP client。烘焙的 observer 是 session 级转发器
    // （每 turn 覆盖式 set 当前 handler），跨 turn 不陈旧。
    // （fingerprint / AgentPool 缓存逻辑在注入的 primary_llm_factory 内完成。）
    let base_model: Arc<dyn peri_model::Model> = (input.primary_llm_factory)();

    // Todo channel
    let (todo_tx, todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);

    // HITL middleware — reuse auto_classifier model from cache when available
    let auto_classifier_model: Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>> = cached_llm
        .map(|c| c.auto_classifier_model.clone())
        .unwrap_or_else(|| (input.auto_classifier_factory)());
    // 其余中间件构造（HITL / AskUser / 父工具集 / SubAgent / 链装配）已随 L2
    // 迁至 peri-middlewares::assembly（链序事实源：Agent 层 session 工厂），
    // 本函数仅构造装配上下文并调用。

    // 子 agent LLM 工厂（支持 SubAgent LLM 缓存复用；注入面，
    // ACP 装配点完成 provider 解析 / fingerprint / 池化）
    let llm_factory = input.llm_factory.clone();

    // 系统提示构建器（注入面；ACP 装配点完成 frozen date / skills 渲染）
    let system_builder = input.system_builder.clone();

    // 后台任务通知通道
    // 装配注入的 per-session TaskManager（L1：BackgroundTaskRegistry per-session
    // 实例化，经 Arc<dyn TaskManager> downcast 还原）。无注入时（NoopTaskManager
    // 降级 / print mode）回退临时实例：AssemblyContext.task_manager 为必填
    // Arc（装配契约），SubAgentMiddleware 依赖它注册子 agent（行为契约，
    // 见 ARC-MIDDLEWARE-001 装配面）。
    let task_manager = task_manager.unwrap_or_else(|| Arc::new(TaskManager::new()));

    // 后台任务完成事件的独立通道（不随 executor 生命周期销毁）
    let (bg_event_tx, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();

    let claude_md_excludes = input.claude_md_excludes.clone();

    // 上下文预算
    let context_window = input.context_window;
    let compact_config = input.compact_config.clone();
    let context_budget = ContextBudget::new(context_window)
        .with_auto_compact_threshold(compact_config.auto_compact_threshold)
        .with_warning_threshold(compact_config.micro_compact_threshold);

    // Git Attribution 已迁移到 GitAttributionMiddleware::prompt_contribution()，
    // 不再手动拼接到 system_prompt。

    // 构造装配上下文并调 Agent 层 session 工厂构建中间件链（L2 归位）。
    // - 唯一触发点：`crate::session::factory::build_middleware_chain`
    //   （session 初始化装配入口；链序事实源 `production_blueprint` 同处，
    //   ARC-MIDDLEWARE-001，顺序是行为契约，禁止重排）
    // - 装配实现：`peri-middlewares::assembly::ProductionChainAssembler`
    //   （含 SubAgentMiddleware 构造点；经 `MiddlewareChainAssembler` trait
    //   注入，本模块不引用装配实现）
    let ChainAssembly {
        chain,
        subagent_mw,
        error_suggest_registry: registry,
        tool_registry_snapshot: snapshot,
    } = assembler.assemble(
        &crate::session::factory::production_blueprint(),
        &AssemblyContext {
            cwd: cwd.clone(),
            cancel: cancel.clone(),
            broker: permission_broker.clone(),
            permission_mode: permission_mode.clone(),
            model_name: input.model_name.clone(),
            provider_name: input.provider_name.clone(),
            auxiliary_model: mw_auxiliary_model.clone(),
            auto_classifier_model: auto_classifier_model.clone(),
            claude_md_excludes,
            preload_skills,
            plugin_skill_roots,
            plugin_loaded,
            hook_groups,
            session_start_source,
            cron_scheduler,
            mcp_pool,
            channel_state,
            tool_search_index,
            shared_tools: shared_tools.clone(),
            lsp_servers,
            lsp_pool: input.lsp_pool.clone(),
            workflow_executor: workflow_executor.clone(),
            workflow_middleware,
            event_handler: Arc::clone(&event_handler),
            task_manager,
            bg_event_tx: bg_event_tx.clone(),
            on_bg_complete,
            // SubAgent Langfuse bridge：注入工厂构造（采样决策继承自父 agent）。
            langfuse_bridge: input.langfuse_bridge_factory.as_ref().map(|f| f()),
            thread_store,
            parent_thread_id,
            register_runtime,
            deregister_runtime,
            child_handler_factory,
            frozen_claude_md,
            frozen_claude_local_md,
            frozen_skill_summary,
            system_prompt_for_sub,
            llm_factory,
            system_builder,
            todo_tx,
            goal_controller,
        },
    );

    // 收集中间件的 prompt_contribution（AgentsMd / Skills / GitAttribution /
    // ToolSearch 等声明式贡献），合并到 system_prompt 后传入 LLM。
    let contributions = chain.collect_prompt_contributions();
    let merged_system_prompt = if contributions.is_empty() {
        system_prompt.clone()
    } else {
        format!("{system_prompt}\n\n{contributions}")
    };

    // 构造 AgentModelBridge（带系统提示词）
    let mut base_llm = AgentModelBridge::new(base_model).with_system(merged_system_prompt);
    if let Some(ref sid) = session_id {
        base_llm = base_llm.with_session_id(sid);
    }
    let model: Arc<dyn ReactLLM + Send + Sync> = Arc::new(base_llm);

    // 构建 CachedLlmInstances 供跨 prompt 复用
    let auxiliary_model_for_cache: Option<Arc<dyn peri_model::Model>> = mw_auxiliary_model.clone();
    let new_cache = auxiliary_model_for_cache.map(|model| CachedLlmInstances {
        auxiliary_model: model,
        auto_classifier_model,
        fingerprint: input.provider_fp.clone(),
    });

    // Session 级 registry 无需本地 channel 清理
    //（session 创建时创建 bg_notification channel，由 session 管理生命周期）

    let components = AgentComponents {
        llm: model,
        chain,
        shared_tools: Some(Arc::clone(&shared_tools)),
        error_suggest_registry: registry,
        tool_registry_snapshot: snapshot,
        context_budget: Some(context_budget),
        compact_config: Some(compact_config),
        subagent_mw,
    };

    (
        AcpAgentOutput {
            components,
            todo_rx,
            bg_event_rx,
            bg_event_tx,
        },
        new_cache,
    )
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
// chain.collect_tools(cwd) 把 middleware 提供的工具 + register_tool 注册的
// AskUserQuestion 一次性 merge 到 shared_tools（已存在的同名工具不覆盖，
// 保留 deferred / 外部注册版本）。
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
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn build_stage_context(
    input: &StageBuildInput,
    assembler: &dyn MiddlewareChainAssembler<Context = AssemblyContext, Output = ChainAssembly>,
    cached_llm: Option<&CachedLlmInstances>,
    system_prompt: String,
    subagent_system_prompt: Option<String>,
    frozen: FrozenData,
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<ChildHandlerFactory>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    thread_persistence: ThreadPersistence,
    goal_controller: Option<Arc<dyn GoalController>>,
    task_manager: Option<Arc<TaskManager>>,
    on_bg_complete: Option<OnBgCompleteFn>,
) -> (V2AgentOutput, Option<CachedLlmInstances>) {
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
        system_prompt,
        subagent_system_prompt.clone(),
        frozen.clone(),
        event_handler,
        agent_overrides,
        preload_skills,
        child_handler_factory,
        auxiliary_model,
        thread_persistence.clone(),
        goal_controller,
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

    // 构造 v2 Session（复用外部 cancel token + 会话级共享 MessageQueue）
    let cwd_arc: Arc<str> = Arc::from(cwd.as_str());
    let frozen_ctx = FrozenContext::builder().build();
    let cancel_arc = Arc::new(cancel_token);
    let session = Session::new_with_cancel_and_queue(
        cwd_arc,
        frozen_ctx,
        None,
        cancel_arc.clone(),
        shared_queue.clone(),
    );

    // 激活 transcript persistence（compact flags 跨 prompt 持久化）
    if let (Some(store), Some(tid)) = (input.thread_store.as_ref(), input.thread_id.as_ref()) {
        let transcript_arc = session.transcript();
        let mut transcript = transcript_arc.write();
        let old = std::mem::take(&mut *transcript);
        *transcript = old.with_persistence(store.clone(), tid.clone());
    }

    // Async Owners（SessionInbox + CronOwner）
    //
    // Session 级路径（TUI/stdio 交互，存在 SessionManager）：cron bridge 由
    // SessionManager::cron_bridge_for 在 AcpSession 上懒启动，跨 turn 存活——
    // turn 结束（含 retry Error）不再杀死 bridge
    // （spec/issues/2026-08-04-cron-trigger-lost-after-turn-error.md）。
    // 此处不再挂载 turn 级 CronOwner，也不调用 set_async_owners
    // （AsyncOwners 容器无生产消费者；executor 的 idle_inbox 走 session 级 inbox）。
    //
    // 无 SessionManager 的路径（print 模式 -p，单次进程）：保留原 turn 级挂载，
    // 行为与现状完全一致。
    if input.launch_cron_bridge.is_some() {
        if let Some(ref launch) = input.launch_cron_bridge {
            launch(&session_id);
        }
    } else if let Some(ref scheduler) = cron_scheduler {
        // ── 原 AsyncOwners 块原样保留（含 per-turn SessionInbox + subscribe +
        //    bridge task + CronOwner + set_async_owners）──
        {
            let shared_queue_arc = Arc::new(shared_queue.clone());
            let session_inbox = SessionInbox::new(shared_queue_arc);
            let inbox_handle = session_inbox.handle();

            let mut trigger_rx = scheduler.subscribe();

            let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
            let shutdown = cancel_arc.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => {
                            break;
                        }
                        trigger = trigger_rx.recv() => {
                            match trigger {
                                Some(t) => {
                                    if prompt_tx.send(t.prompt).is_err() {
                                        tracing::debug!("cron-bridge: prompt_tx closed, stopping");
                                        break;
                                    }
                                }
                                None => {
                                    tracing::debug!("cron-bridge: trigger_rx closed, stopping");
                                    break;
                                }
                            }
                        }
                    }
                }
            });

            let mut owner = CronOwner::new();
            owner.start(prompt_rx, inbox_handle, cancel_arc.clone());
            tracing::info!("CronOwner started (ACP bridge path)");

            // 分支内 scheduler 恒为 Some（else-if 绑定），直接注入
            session.set_async_owners(session_inbox, Some(owner), None);
        }
    }

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

    // 注入父 agent 身份（C2）：SubAgentTool 持有同一共享 cell，
    // invoke 时（必然晚于本调用）读到已 set 的值——共享 cell 消除顺序问题。
    if let Some(mw) = &subagent_mw {
        mw.set_parent_agent_id(main_agent_id);
    }

    // L3：注入子 agent 运行时宿主（SubagentHost）并挂到主 session。
    // SubAgentTool 经 parent_session 读取运行时通道（thread_store / task_manager /
    // bg_event_sender / register / deregister / langfuse）与 frozen 数据回退，
    // SubAgentMiddleware 不再逐字段透传（管理权移出）。
    {
        let host = SubagentHost {
            thread_store: thread_persistence.store.clone(),
            task_manager: task_manager.clone(),
            bg_event_sender: Some(bg_event_tx),
            on_bg_complete: on_bg_complete.clone(),
            register_runtime: thread_persistence.register_runtime.clone(),
            deregister_runtime: thread_persistence.deregister_runtime.clone(),
            // SubAgent Langfuse bridge：注入工厂构造独立 LangfuseBridge 实例
            // （采样决策继承自父 agent）。
            langfuse_bridge: input.langfuse_bridge_factory.as_ref().map(|f| f()),
            // Frozen CLAUDE.local.md 不在 FrozenContext（父 session 无此字段），
            // 由 session/new 冻结数据注入（不重读磁盘）。
            frozen_claude_local_md: frozen
                .claude_local_md
                .as_ref()
                .map(|s| Arc::new(s.to_string())),
            frozen_system_prompt: subagent_system_prompt.as_ref().map(|s| Arc::new(s.clone())),
            parent_thread_id: thread_persistence.parent_thread_id.clone(),
            frozen_claude_md: frozen.claude_md.as_ref().map(|s| Arc::new(s.clone())),
            frozen_skill_summary: frozen.skill_summary.as_ref().map(|s| Arc::new(s.clone())),
        };
        session.set_subagent_host(host);
        // 父 v2 session 注入 SubAgentMiddleware（与 set_parent_agent_id 同点；
        // build_tool 必然晚于本调用，读到已 set 的 session）
        if let Some(mw) = &subagent_mw {
            mw.set_parent_session(session.clone());
        }
    }

    // [时序契约] 工具注入必须晚于 parent_session 注入：SubAgentTool 在
    // build_tool（collect_tools）时读取 parent_session 以获取运行时 host
    // （task_manager / bg_event_sender / thread_store / frozen 回退）——先于
    // 注入则 host 为空，`run_in_background: true` 会静默降级为同步执行
    // （bg subagent 不注册 TaskManager，BgTaskArea 无运行条目，
    // issue 2026-08-06-e2e-bg-task-area-entry-missing）。每轮重建，顺序不可调换。
    // 已存在的同名工具不覆盖（deferred tools 优先保留外部注册版本）。
    {
        let middleware_tools = chain.collect_tools(&cwd);
        let mut tools = shared_tools.write();
        for tool in middleware_tools {
            let arc: Arc<dyn BaseTool> = Arc::from(tool);
            // 使用 insert：有状态工具（如 SubAgentTool）需每 turn 更新。
            tools.insert(arc.name().to_string(), arc);
        }
    }

    // 构造 StageContext（builder 构造晚于工具注入：chain 在
    // collect_tools 借用后被 move 进 builder，顺序不可调换）
    let mut builder = StageContext::builder(turn, transcript, queue)
        .with_agent_id(main_agent_id)
        .with_llm(react_llm)
        .with_tools(shared_tools)
        .with_tool_invocation_resolver(Arc::clone(&input.tool_invocation_resolver))
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::new(event_bus))
        .with_session_context(session_context)
        .with_tool_registry_snapshot((*tool_registry_snapshot).clone());

    if let Some(reg) = error_suggest_registry {
        builder = builder.with_error_suggest_registry(reg);
    }
    if let Some(budget) = context_budget {
        builder = builder.with_context_budget(budget);
    }
    if let Some(cc) = compact_config {
        builder = builder.with_compact_config(cc);
    }
    if let Some(llm) = compact_llm_for_v2 {
        builder = builder.with_compact_llm(llm);
    }
    if let Some(inbox) = idle_inbox {
        builder = builder.with_idle_inbox(inbox);
    }
    if let Some(probe) = idle_should_wait {
        builder = builder.with_idle_should_wait(probe);
    }
    if let Some(flag) = input.idle_suspended_flag.clone() {
        builder = builder.with_idle_suspended_flag(flag);
    }

    // 注入 compact plugin hook 回调（hook_groups 非空时 ACP 装配点构造闭包）
    if let Some(hook) = &input.compact_pre_hook {
        builder = builder.with_compact_pre_hook(Arc::clone(hook));
    }
    if let Some(hook) = &input.compact_post_hook {
        builder = builder.with_compact_post_hook(Arc::clone(hook));
    }

    let context = builder.build();

    (
        V2AgentOutput {
            context,
            session,
            event_handles,
            todo_rx: agent_output.todo_rx,
            bg_event_rx: agent_output.bg_event_rx,
        },
        new_cached,
    )
}

#[cfg(test)]
mod builder_v2_tests {
    use super::*;

    #[test]
    fn test_v2_context_has_null_llm_by_default() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx =
            StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
        assert_eq!(ctx.runtime.llm.model_name(), "null");
    }
}
