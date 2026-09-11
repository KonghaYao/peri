//! 模型缓存、冻结 prompt 与生产链上下文投影。
use super::{AcpAgentOutput, AgentComponents, CachedLlmInstances, StageBuildInput};
use crate::{
    agent::{
        async_tasks::TaskManager, model_bridge::AgentModelBridge, react::ReactLLM,
        token::ContextBudget,
    },
    session::{
        exec::executor::FrozenSessionData,
        factory::{AssemblyContext, ChainAssembly, MiddlewareChainAssembler, OnBgCompleteFn},
    },
};
use peri_acp_types::{
    agents::AgentOverrides,
    event::AgentEventHandler,
    frozen::{ChildHandlerFactory, ThreadPersistence},
    goal::GoalController,
    tools::TodoItem,
};
use std::sync::Arc;

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
    frozen_session: &FrozenSessionData,
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
    // FrozenContext 中的空字符串是“冻结缺席”，仍须投影为 Some("")，
    // 防止 AgentsMd/Skills middleware 在 before_agent 阶段重读晚到文件。
    let frozen = frozen_session.v2_frozen();
    let frozen_claude_md = Some(frozen.claude_md.to_string());
    let frozen_claude_local_md = frozen_session.claude_local_md().map(ToString::to_string);
    let frozen_skill_summary = Some(frozen.skill_summary.to_string());
    let system_prompt = frozen.system_prompt.to_string();

    // 从 StageBuildInput 提取共享字段
    let cwd = input.cwd.clone();
    let session_id = Some(input.session_id.clone());
    let shared_tools = input.shared_tools.clone();
    let mw_auxiliary_model = auxiliary_model;

    // Retry observer 转发器（session 级，挂 AgentPool）：本 turn 的 event_handler
    // 在构造模型前覆盖式 set，池化模型烘焙转发器引用，发射时读取当前 turn 的
    // 最新 handler——跨 turn 不陈旧。
    let retry_events = input.retry_events.clone();
    retry_events.set(Some(Arc::clone(&event_handler)));

    // Capture system_prompt before it may be overridden below (for SubAgent fork reuse).
    // 16_workflow 已删除（C2）：子面向 prompt 与主 prompt 字节相同（无二次
    // 渲染版本），直接复用主 prompt。
    let system_prompt_for_sub = system_prompt.clone();

    // 应用 agent overrides 到系统提示词
    let system_prompt = agent_overrides.as_ref().map_or_else(
        || system_prompt.clone(),
        |ov| (input.render_system_prompt)(Some(ov), &cwd),
    );

    // 提前提取模型实例（chain 构建完成后才组装 AgentModelBridge，
    // 以便 bridge provider 与 StageContext 共享同一 Arc<MiddlewareChain>；
    // contribution 在 before_agent 后按 ModelRequest 同步收集）。
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

    // 后台任务通知通道
    // 装配注入的 per-session TaskManager（L1：BackgroundTaskRegistry per-session
    // 实例化，经 Arc<dyn TaskManager> downcast 还原）。无注入时（NoopTaskManager
    // 降级 / print mode）回退临时实例：AssemblyContext.task_manager 为必填
    // Arc（装配契约），SubAgentMiddleware 依赖它注册子 agent（行为契约，
    // 见 ARC-MIDDLEWARE-001 装配面）。
    let task_manager = task_manager.unwrap_or_else(|| Arc::new(TaskManager::new()));

    // 后台任务完成事件的独立通道（不随 executor 生命周期销毁）
    let (bg_event_tx, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();

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
        &project_assembly(
            input,
            TurnAssembly {
                event_handler: Arc::clone(&event_handler),
                agent_overrides: agent_overrides.clone(),
                preload_skills,
                child_handler_factory,
                mw_auxiliary_model: mw_auxiliary_model.clone(),
                auto_classifier_model: auto_classifier_model.clone(),
                thread_persistence,
                goal_controller,
                task_manager,
                on_bg_complete,
                todo_tx,
                bg_event_tx: bg_event_tx.clone(),
                frozen_claude_md,
                frozen_claude_local_md,
                frozen_skill_summary,
                system_prompt_for_sub,
            },
        ),
    );

    // bridge 与 StageContext 必须共享同一条 middleware chain：before_agent
    // 填充的 session-local cache 由下一个 ModelRequest 在同步构造阶段读取。
    let chain = Arc::new(chain);
    let contribution_chain = Arc::clone(&chain);

    // 构造 AgentModelBridge（冻结 base 不变；动态 contribution request-time 组合）
    let mut base_llm = AgentModelBridge::new(base_model)
        .with_system(system_prompt)
        .with_system_contribution_provider(Arc::new(move || {
            contribution_chain.collect_prompt_contributions()
        }));
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

/// 单次链装配的资源投影；不承担新的 session 所有权。
struct TurnAssembly {
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<ChildHandlerFactory>,
    mw_auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    auto_classifier_model: Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>>,
    thread_persistence: ThreadPersistence,
    goal_controller: Option<Arc<dyn GoalController>>,
    task_manager: Arc<TaskManager>,
    on_bg_complete: Option<OnBgCompleteFn>,
    todo_tx: tokio::sync::mpsc::Sender<Vec<TodoItem>>,
    bg_event_tx: super::BgEventTx,
    frozen_claude_md: Option<String>,
    frozen_claude_local_md: Option<String>,
    frozen_skill_summary: Option<String>,
    system_prompt_for_sub: String,
}

fn project_assembly(input: &StageBuildInput, turn: TurnAssembly) -> AssemblyContext {
    let TurnAssembly {
        event_handler,
        agent_overrides,
        preload_skills,
        child_handler_factory,
        mw_auxiliary_model,
        auto_classifier_model,
        thread_persistence,
        goal_controller,
        task_manager,
        on_bg_complete,
        todo_tx,
        bg_event_tx,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        system_prompt_for_sub,
    } = turn;
    let ThreadPersistence {
        store: thread_store,
        parent_thread_id,
        register_runtime,
        deregister_runtime,
    } = thread_persistence;
    AssemblyContext {
        cwd: input.cwd.clone(),
        cancel: input.cancel.clone(),
        broker: input.broker.clone(),
        permission_mode: input.permission_mode.clone(),
        model_name: input.model_name.clone(),
        provider_name: input.provider_name.clone(),
        auxiliary_model: mw_auxiliary_model,
        auto_classifier_model,
        claude_md_excludes: input.claude_md_excludes.clone(),
        preload_skills,
        plugin_skill_roots: input.plugin_skill_roots.clone(),
        plugin_loaded: input.plugin_loaded.clone(),
        hook_groups: input.hook_groups.clone(),
        session_start_source: input.session_start_source.clone(),
        mcp_skill_registry: input.mcp_skill_registry.clone(),
        command_registry: input.command_registry.clone(),
        cron_scheduler: input.cron_scheduler.clone(),
        mcp_pool: input.mcp_pool.clone(),
        dynamic_mcp: input.dynamic_mcp.clone(),
        dynamic_mcp_projection: Arc::clone(&input.dynamic_mcp_projection),
        session_id: input.session_id.clone(),
        channel_state: input.channel_state.clone(),
        tool_search_index: input.tool_search_index.clone(),
        shared_tools: input.shared_tools.clone(),
        // MetaHarness：装配期关闭集合（源自会话冻结状态投影，
        // 顶层链过滤——设计 §2.5；禁止从每 turn 当前配置重建）。
        meta_harness_disabled: input.meta_harness_disabled.clone(),
        // 波 4 演进 2：基础段持有者（DefaultSystemPromptMiddleware 的
        // persona 内容源 = 与 render_system_prompt 同一份 agent_overrides；
        // LangMiddleware 的语言内容源 = 冻结语言，保证链收集与渲染一致）。
        agent_overrides,
        language: input.language.clone(),
        lsp_servers: input.lsp_servers.clone(),
        lsp_pool: input.lsp_pool.clone(),
        workflow_executor: input.workflow_executor.clone(),
        workflow_middleware: input.workflow_middleware.clone(),
        event_handler,
        task_manager,
        bg_event_tx,
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
        llm_factory: input.llm_factory.clone(),
        system_builder: input.system_builder.clone(),
        todo_tx,
        goal_controller,
    }
}
