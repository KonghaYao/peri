//! Shared Agent builder（ACP 和 TUI 共用）
//!
//! 提供 `build_agent()` 构建函数：构造装配上下文，经 Agent 层 session 工厂
//! 构建中间件链，并产出 `AgentComponents`（供 v2 builder 消费）。
//! 链装配实现已随 L2 迁出（见下方模块注释）。
//!
//! 本模块从 peri-tui/src/app/agent.rs:build_bare_agent() 迁移而来，
//! 删除 TUI 特有依赖（ExecutorEvent channel、map_executor_event），
//! 改为通过 `child_handler_factory` 参数从外部注入。

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::{
    compact::CompactConfig,
    event::{AgentEventHandler, ExecutorEvent},
    event_v2::{EventBus, EventBusConfig, EventHandles},
    frozen::{ChildHandlerFactory, FrozenData, ThreadPersistence},
    identity::AgentId,
    session::{CronOwner, SessionInbox},
};
use peri_middlewares::{
    assembly::{
        AssemblyContext, ChainAssembly, OnBgCompleteFn, ProductionChainAssembler,
        SystemPromptBuilder,
    },
    tools::TodoItem,
};

use crate::{
    provider::LlmProvider,
    session::agent_pool::{fingerprint, CachedLlmInstances},
};
use peri_controller::langfuse::bridge::LangfuseBridge;
use peri_controller::langfuse::tracer::LangfuseTracer;

// ── 共享 Agent 构建（ACP 和 TUI 共用）─────────────────────────────────────────
//
// 链装配（含 SubAgentMiddleware 构造点）已随 L2 迁出：
// - 唯一触发点与链序事实源：peri-agent/src/session/factory.rs 的
//   `peri_agent::session::factory::build_middleware_chain` + `production_blueprint`（ARC-MIDDLEWARE-001）
// - 装配实现：peri-middlewares/src/assembly.rs 的 `ProductionChainAssembler`
// - 随迁类型：`FrozenData` / `ThreadPersistence` / `ChildHandlerFactory` /
//   `RegisterRuntimeFn` / `DeregisterRuntimeFn` 位于
//   peri_agent::session::factory

pub(crate) struct AcpAgentOutput {
    pub components: AgentComponents,
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<TodoItem>>,
    /// 后台任务完成事件的独立接收端（不随 executor 生命周期销毁）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
    /// 后台任务完成事件的发送端（L3：注入 SubagentHost，子 agent bg 事件经此
    /// 通道到达 executor_helpers 的 bg event pump）
    pub bg_event_tx: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
}

/// Agent 装配产物（v2 builder 直接消费，P5.3 抽取）
///
/// `build_agent` 经 Agent 层 session 工厂装配 `peri_agent::middleware::chain::MiddlewareChain`，
/// 并组装 LLM + system prompt 等字段产出本结构，
/// `build_stage_context` 消费它构造 v2 `peri_agent::agent::stages::StageContext`。
pub struct AgentComponents {
    /// 主 LLM（已通过 `peri_agent::agent::model_bridge::AgentModelBridge` 适配为标准 ReAct 抽象）
    pub llm: Arc<dyn peri_agent::agent::react::ReactLLM + Send + Sync>,
    /// 中间件链（v2 peri_agent::agent::stages::StageContext 直接复用）
    pub chain: peri_agent::middleware::chain::MiddlewareChain,
    /// 共享工具注册表（deferred tools，供 ExecuteExtraTool 代理）
    #[allow(clippy::type_complexity)]
    pub shared_tools:
        Option<Arc<parking_lot::RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>>,
    /// 错误感知建议注册表
    pub error_suggest_registry: Option<Arc<peri_agent::error_suggest::ErrorSuggestRegistry>>,
    /// 工具注册表快照（工具名 + subagent 类型）
    pub tool_registry_snapshot: Arc<peri_agent::error_suggest::ToolRegistrySnapshot>,
    /// 上下文预算（token 监控）
    pub context_budget: Option<peri_agent::agent::token::ContextBudget>,
    /// Compact 配置
    pub compact_config: Option<CompactConfig>,
    /// SubAgent 中间件（chain 中已有一份 clone；本字段保留原实例，
    /// 供 build_stage_context 在主 v2 session 创建后注入 parent_agent_id）
    pub subagent_mw: Option<peri_middlewares::subagent::SubAgentMiddleware>,
}

/// 构建可复用的 Agent（ACP 和 TUI 共用核心构建逻辑）
///
/// 迁移自 peri-tui/src/app/agent.rs:build_bare_agent()。
/// 中间件链装配经 Agent 层 session 工厂（唯一触发点 `peri_agent::session::factory::build_middleware_chain`，
/// 链序蓝本 `production_blueprint`，ARC-MIDDLEWARE-001）与
/// `peri-middlewares::assembly::ProductionChainAssembler` 完成，
/// 本函数构造装配上下文并组装 LLM/prompt/缓存。
///
/// `cached_llm` 允许跨 prompt 复用 LLM 实例（auxiliary_model、auto_classifier_model），
/// 避免每轮重建 reqwest::Client（~1-2 MB/实例）。首次调用传 `None`，
/// 后续调用传上一次返回的 `Some(CachedLlmInstances)`。
///
/// `pool` 提供 SubAgent LLM 缓存，跨 SubAgent 调用复用 `Arc<dyn peri_model::Model>`
/// （含共享的 `reqwest::Client`）。首次同模型 SubAgent 调用时创建新实例并插入缓存，
/// 后续调用直接命中缓存，避免每 SubAgent 分配 ~1-2 MB 的 HTTP client。
#[allow(clippy::too_many_arguments)] // 过渡：AAC 字段已拆分为独立参数
pub(crate) fn build_agent(
    ctx: &crate::session::executor::SessionContext,
    system_prompt: String,
    subagent_system_prompt: Option<String>,
    frozen: FrozenData,
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<peri_middlewares::agent_define::AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<ChildHandlerFactory>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    thread_persistence: ThreadPersistence,
    goal_controller: Option<Arc<dyn peri_acp_types::goal::GoalController>>,
    task_manager: Option<Arc<peri_agent::agent::async_tasks::TaskManager>>,
    on_bg_complete: Option<OnBgCompleteFn>,
    cached_llm: Option<&CachedLlmInstances>,
    langfuse_tracer: Option<Arc<parking_lot::Mutex<LangfuseTracer>>>,
) -> (AcpAgentOutput, Option<CachedLlmInstances>) {
    let FrozenData {
        claude_md: frozen_claude_md,
        claude_local_md: frozen_claude_local_md,
        skill_summary: frozen_skill_summary,
        date: frozen_date,
    } = frozen;

    let ThreadPersistence {
        store: thread_store,
        parent_thread_id,
        register_runtime,
        deregister_runtime,
    } = thread_persistence;

    // 从 SessionContext 提取共享字段
    let provider = ctx.provider.clone();
    let cwd = ctx.cwd.clone();
    let cancel = ctx.cancel.clone();
    let permission_mode = ctx.permission_mode.clone();
    let peri_config = ctx.peri_config.clone();
    let cron_scheduler = ctx.cron_scheduler.clone().and_then(|s| {
        s.downcast_arc::<peri_middlewares::cron::CronSchedulerPortHandle>()
            .ok()
            .map(|handle| handle.0.clone())
    });
    let session_id = Some(ctx.session_id.clone());
    let permission_broker = ctx.broker.clone();
    let plugin_skill_roots = ctx.plugin_skill_roots.clone();
    let plugin_agent_dirs = ctx.plugin_agent_dirs.clone();
    let plugin_loaded = ctx.plugin_loaded.clone();
    let hook_groups = ctx.hook_groups.clone();
    let session_start_source = ctx.session_start_source.clone();
    let mcp_pool = ctx.mcp_pool.clone().and_then(|s| {
        s.downcast_arc::<peri_middlewares::mcp::McpClientPool>()
            .ok()
    });
    let channel_state = ctx.channel_state.clone();
    let tool_search_index = match ctx
        .tool_search_index
        .clone()
        .downcast_arc::<peri_middlewares::tool_search::ToolSearchIndex>()
    {
        Ok(idx) => idx,
        Err(_) => Arc::new(peri_middlewares::tool_search::ToolSearchIndex::default()),
    };
    let shared_tools = ctx.shared_tools.clone();
    let lsp_servers = ctx.lsp_servers.clone();
    let workflow_executor = ctx.workflow_executor.clone();
    let workflow_middleware = ctx.workflow_middleware.clone().and_then(|s| {
        s.downcast_arc::<peri_middlewares::workflow::WorkflowMiddleware>()
            .ok()
    });
    let mw_auxiliary_model = auxiliary_model;
    let pool = &ctx.pool;

    // Retry observer 转发器（session 级，挂 AgentPool）：本 turn 的 event_handler
    // 在构造模型前覆盖式 set，池化模型烘焙转发器引用，发射时读取当前 turn 的
    // 最新 handler——跨 turn 不陈旧。
    let retry_events = ctx.pool.lock().retry_events.clone();
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
        |ov| {
            // workflow_enabled 与下方 WorkflowMiddlewareAdaptor 条件注册共用
            // 同一条件源（workflow_executor.is_some()），保证 prompt 声明与
            // 工具注册一致（阶段 3 capability 契约）。
            let features = crate::prompt::PromptFeatures::detect(
                permission_mode.load(),
                workflow_executor.is_some(),
            );
            let template = crate::prompt::PromptTemplate::with_overrides(ov);
            let env = crate::prompt::PromptEnv::detect(&cwd);
            template.render(
                &env,
                &features,
                ctx.skills.as_ref(),
                &plugin_agent_dirs,
                None,
            )
        },
    );

    let provider_for_factory = provider.clone();
    let model_name = provider.model_name().to_string();
    let provider_name = provider.display_name().to_string();

    // 提前提取模型实例（chain 构建完成后才组装 peri_agent::agent::model_bridge::AgentModelBridge，
    // 以便收集中间件 prompt_contribution 合并到 system prompt）。
    // 与 SubAgent 模型共享 session 级 AgentPool 缓存（同一 fingerprint）：
    // 跨 turn / 跨 agent 实例复用 reqwest::Client（连接池 + TLS session cache），
    // 避免每轮重建 ~1-2 MB HTTP client。烘焙的 observer 是 session 级转发器
    // （每 turn 覆盖式 set 当前 handler），跨 turn 不陈旧。
    let context_window_raw = ctx.provider.context_window();
    let fp = fingerprint(&provider);
    let base_model: Arc<dyn peri_model::Model> =
        crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(pool, &fp, || {
            provider
                .clone()
                .with_retry_observer(Some(retry_events.as_retry_observer()))
                .into_model()
        });

    // Todo channel
    let (todo_tx, todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);

    // HITL middleware — reuse auto_classifier model from cache when available
    let auto_classifier_model: Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>> = cached_llm
        .map(|c| c.auto_classifier_model.clone())
        .unwrap_or_else(|| {
            Arc::new(tokio::sync::Mutex::new(
                provider_for_factory
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model(),
            ))
        });
    // 其余中间件构造（HITL / AskUser / 父工具集 / SubAgent / 链装配）已随 L2
    // 迁至 peri-middlewares::assembly（链序事实源：Agent 层 session 工厂），
    // 本函数仅构造装配上下文并调用。

    // 子 agent LLM 工厂（支持 SubAgent LLM 缓存复用）
    let provider_fp = fingerprint(&provider_for_factory);
    let provider_clone = provider_for_factory;
    let config_for_factory = peri_config.clone();
    let session_id_for_factory = session_id.clone();
    let pool_for_subagent = Arc::clone(pool);
    #[allow(clippy::type_complexity)]
    let llm_factory: Arc<
        dyn Fn(Option<&str>) -> Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync>
            + Send
            + Sync,
    > = Arc::new(move |model_alias: Option<&str>| {
        let sid = session_id_for_factory.as_deref();
        // 解析 provider 并构建 fingerprint
        let (p, fp) = if let Some(alias) = model_alias {
            match LlmProvider::from_config_for_alias(&config_for_factory, alias) {
                Some(p) => {
                    let fp = fingerprint(&p);
                    (Some(p), fp)
                }
                None => {
                    let fp = fingerprint(&provider_clone);
                    (None, fp)
                }
            }
        } else {
            let fp = fingerprint(&provider_clone);
            (None, fp)
        };

        // 尝试 SubAgent 缓存
        let model: Arc<dyn peri_model::Model> =
            crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(
                &pool_for_subagent,
                &fp,
                || match &p {
                    Some(provider) => provider
                        .clone()
                        .with_retry_observer(Some(retry_events.as_retry_observer()))
                        .into_model(),
                    None => provider_clone
                        .clone()
                        .with_retry_observer(Some(retry_events.as_retry_observer()))
                        .into_model(),
                },
            );

        let mut llm = peri_agent::agent::model_bridge::AgentModelBridge::from_arc(model);
        if let Some(s) = sid {
            llm = llm.with_session_id(s);
        }
        Box::new(llm)
    });

    // 系统提示构建器
    let frozen_language_for_sub = peri_config.config.language.clone();
    let frozen_date_for_sub = frozen_date.clone();
    let skills_for_sub = ctx.skills.clone();
    // PromptFeatures is detected at build-time: hitl 来自 permission mode，
    // workflow 对子 agent / fork 恒为 false（detect_without_workflow）——
    // 这些链不注册 WorkflowTool、shared_tools 为 None，不得宣称 workflow
    // 可用（P2-2026-08-02）；主 agent 的 workflow 声明由
    // `workflow_executor.is_some()` 独立控制（builder.rs 条件注册同源）。
    let features_for_sub =
        crate::prompt::PromptFeatures::detect_without_workflow(permission_mode.load());
    let template_for_sub = crate::prompt::PromptTemplate::new();
    let system_builder: SystemPromptBuilder = Arc::new(move |overrides, cwd_dir| {
        let t = overrides.map_or_else(
            || template_for_sub.clone(),
            crate::prompt::PromptTemplate::with_overrides,
        );
        let env = if let Some(ref date) = frozen_date_for_sub {
            crate::prompt::PromptEnv::with_frozen_date(cwd_dir, date)
        } else {
            crate::prompt::PromptEnv::detect(cwd_dir)
        };
        t.render(
            &env,
            &features_for_sub,
            skills_for_sub.as_ref(),
            &[],
            frozen_language_for_sub.as_deref(),
        )
    });

    // 后台任务通知通道
    // 装配注入的 per-session TaskManager（L1：BackgroundTaskRegistry per-session
    // 实例化，经 Arc<dyn TaskManager> downcast 还原）。无注入时（NoopTaskManager
    // 降级 / print mode）回退临时实例：AssemblyContext.task_manager 为必填
    // Arc（peri-middlewares::assembly 契约），SubAgentMiddleware 依赖它注册
    // 子 agent（行为契约，见 ARC-MIDDLEWARE-001 装配面）；回退构造点随 L5
    // executor 拆分迁入 Agent 层 session 工厂（豁免清单见
    // `spec/issues/2026-08-05-3.0-acp-events-session-batch2.md`）。
    let task_manager = task_manager
        .unwrap_or_else(|| Arc::new(peri_agent::agent::async_tasks::TaskManager::new()));

    // 后台任务完成事件的独立通道（不随 executor 生命周期销毁）
    let (bg_event_tx, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();

    let claude_md_excludes = peri_config
        .config
        .claude_md_excludes
        .clone()
        .unwrap_or_default();

    // 上下文预算
    let mut context_window = context_window_raw;
    let context_1m = ctx.provider.context_1m();
    if context_1m {
        context_window = 1_000_000;
    }
    let mut compact_config = peri_config.config.compact.clone().unwrap_or_default();
    compact_config.apply_env_overrides();
    let context_budget = peri_agent::agent::token::ContextBudget::new(context_window)
        .with_auto_compact_threshold(compact_config.auto_compact_threshold)
        .with_warning_threshold(compact_config.micro_compact_threshold);

    // Git Attribution 已迁移到 GitAttributionMiddleware::prompt_contribution()，
    // 不再手动拼接到 system_prompt。

    // 构造装配上下文并调 Agent 层 session 工厂构建中间件链（L2 归位）。
    // - 唯一触发点：peri-agent/src/session/factory.rs 的 `peri_agent::session::factory::build_middleware_chain`
    //   （session 初始化装配入口；链序事实源 `production_blueprint` 同处，
    //   ARC-MIDDLEWARE-001，顺序是行为契约，禁止重排）
    // - 装配实现：peri-middlewares/src/assembly.rs 的 `ProductionChainAssembler`
    //   （含 SubAgentMiddleware 构造点；ACP 侧不再有中间件装配代码，
    //   仅投影装配上下文数据——上下文构造归位属 L5 session 工厂收口）
    let ChainAssembly {
        chain,
        subagent_mw,
        error_suggest_registry: registry,
        tool_registry_snapshot: snapshot,
    } = peri_agent::session::factory::build_middleware_chain(
        &ProductionChainAssembler,
        &AssemblyContext {
            cwd: cwd.clone(),
            cancel: cancel.clone(),
            broker: permission_broker.clone(),
            permission_mode: permission_mode.clone(),
            model_name,
            provider_name,
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
            workflow_executor: workflow_executor.clone(),
            workflow_middleware,
            event_handler: Arc::clone(&event_handler),
            task_manager,
            bg_event_tx: bg_event_tx.clone(),
            on_bg_complete,
            // SubAgent Langfuse bridge：复用父 agent 的 LangfuseTracer 构造独立
            // LangfuseBridge 实例（采样决策继承自父 agent，bridge 内部调用
            // tracer.on_* 方法时各方法已内置 sampling.should_emit() 检查）。
            langfuse_bridge: langfuse_tracer.as_ref().map(|tracer| {
                let bridge = LangfuseBridge::new(
                    Arc::clone(tracer),
                    provider.display_name().to_string(),
                    // SubAgent forwarder bridge:不注入 main_agent_id(child 事件按 registry 归属)
                    None,
                );
                Arc::new(bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>
            }),
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

    // 构造 peri_agent::agent::model_bridge::AgentModelBridge（带系统提示词）
    let mut base_llm = peri_agent::agent::model_bridge::AgentModelBridge::new(base_model)
        .with_system(merged_system_prompt);
    if let Some(ref sid) = session_id {
        base_llm = base_llm.with_session_id(sid);
    }
    let model: Arc<dyn peri_agent::agent::react::ReactLLM + Send + Sync> = Arc::new(base_llm);

    // 构建 CachedLlmInstances 供跨 prompt 复用
    let auxiliary_model_for_cache: Option<Arc<dyn peri_model::Model>> = mw_auxiliary_model.clone();
    let new_cache = auxiliary_model_for_cache.map(|model| CachedLlmInstances {
        auxiliary_model: model,
        auto_classifier_model,
        fingerprint: provider_fp.clone(),
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

// ── v2 peri_agent::agent::stages::StageContext 构建（合并自 builder_v2.rs）────────────────────────────────
//
// 直接构造 peri_agent::agent::stages::StageContext 供 run_react_loop 消费。
// 复用上方 build_agent() 的中间件链与 LLM 构造（AgentComponents），避免重复 700+ 行装配逻辑。
//
// ## 工具注入
//
// run_react_loop 每轮从 shared_tools（peri_agent::agent::stages::SharedToolMap）按名读取工具，
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
// 本函数不再挂载 turn 级 CronOwner。
//
// 仅 print 模式（-p，无 SessionManager）走本函数的 turn 级挂载：
// 1. 创建 SessionInbox（await-wake wrapper around shared_queue）。
// 2. 从 CronScheduler 订阅 trigger_rx（通过 subscribe()）。
// 3. 启动 CronTrigger→String 桥接任务。
// 4. 创建并启动 CronOwner（trigger_rx → inbox）。
// 5. 通过 Session::set_async_owners 注入到 Session。
//
// （事件总线 / CronOwner / SessionInbox 经 `peri_acp_types` 契约面导入；
// 执行面类型（StageContext / ReactLLM / TaskManager 等）保留全路径引用，
// 随 L5 executor 拆分物理迁入 peri-agent。）

/// 为 ACP 生产 peri_agent::agent::stages::StageContext 安装 wrapper-aware canonical invocation resolver。
fn install_tool_invocation_resolver(
    builder: peri_agent::agent::stages::StageContextBuilder,
) -> peri_agent::agent::stages::StageContextBuilder {
    builder.with_tool_invocation_resolver(Arc::new(
        peri_middlewares::ExecuteExtraToolResolver::default(),
    ))
}

/// v2 builder 产物
pub(crate) struct V2AgentOutput {
    /// 已配置的 peri_agent::agent::stages::StageContext（用于 run_react_loop）
    pub context: peri_agent::agent::stages::StageContext,
    /// v2 Session（持有 transcript + queue + store）
    pub session: Arc<peri_agent::session::Session>,
    /// EventBus 消费端（转 ExecutorEvent 用）
    pub event_handles: EventHandles,
    /// Todo 更新通道（spawn todo forwarder 用）
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<peri_middlewares::tools::TodoItem>>,
    /// 后台任务完成事件接收端（spawn bg event pump 用）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
}

/// 从 SessionContext 构造 peri_agent::agent::stages::StageContext
///
/// 内部调用 build_agent 提取 middleware chain + LLM + 共享组件（AgentComponents），
/// 然后构造 peri_agent::agent::stages::StageContext。
///
/// **shared_queue**：会话级共享的 v2 MessageQueue。每个 turn 调用本函数时
/// 必须传入**同一个**实例（来自 AcpSession.v2_message_queue），让本 turn 的
/// peri_agent::agent::stages::StageContext.queue 与会话级共享。
///
/// MessageQueue 内部 Arc<Mutex<VecDeque>> + Arc<Notify>，clone 共享底层；
/// 传入引用只是为了避免在签名里 move。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub(crate) fn build_stage_context(
    ctx: &crate::session::executor::SessionContext,
    cached_llm: Option<&CachedLlmInstances>,
    system_prompt: String,
    subagent_system_prompt: Option<String>,
    frozen: FrozenData,
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<peri_middlewares::agent_define::AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<ChildHandlerFactory>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    thread_persistence: ThreadPersistence,
    goal_controller: Option<Arc<dyn peri_acp_types::goal::GoalController>>,
    task_manager: Option<Arc<peri_agent::agent::async_tasks::TaskManager>>,
    on_bg_complete: Option<OnBgCompleteFn>,
    langfuse_tracer: Option<Arc<parking_lot::Mutex<LangfuseTracer>>>,
) -> (V2AgentOutput, Option<CachedLlmInstances>) {
    // 提取 LLM 用字段（在 cfg 被 build_agent 消费前）
    let cwd = ctx.cwd.clone();
    let session_id = ctx.session_id.clone();
    let cancel_token = ctx.cancel.clone();
    // compact_llm：优先取 auxiliary_model，否则回落到 cached auxiliary_model。
    let compact_llm_for_v2 = auxiliary_model
        .clone()
        .or_else(|| cached_llm.map(|c| c.auxiliary_model.clone()));

    // 提取 hooks 和模型名
    let hook_groups_flat: Vec<peri_middlewares::hooks::types::RegisteredHook> =
        ctx.hook_groups.iter().flatten().cloned().collect();
    let hook_model = ctx.provider.model_name().to_string();
    let hook_session_id = session_id.clone();

    // 提取 cron_scheduler（装配注入端口 → 具体类型，downcast 还原）
    let cron_scheduler = ctx.cron_scheduler.clone().and_then(|s| {
        s.downcast_arc::<peri_middlewares::cron::CronSchedulerPortHandle>()
            .ok()
            .map(|handle| handle.0.clone())
    });

    // 从 SessionContext 推导会话级共享变量
    let shared_queue = ctx
        .session_manager
        .as_ref()
        .and_then(|sm| sm.v2_queue_for(&ctx.session_id))
        .unwrap_or_default();

    let session_inbox_from_mgr = ctx
        .session_manager
        .as_ref()
        .and_then(|sm| sm.session_inbox_for(&ctx.session_id));

    let idle_inbox: Option<Arc<SessionInbox>> = if ctx.allow_await_wake {
        session_inbox_from_mgr.as_ref().map(Arc::clone)
    } else {
        None
    };

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
        ctx,
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
        langfuse_tracer.clone(),
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

    let shared_tools: peri_agent::agent::stages::SharedToolMap = shared_tools_opt
        .unwrap_or_else(|| Arc::new(RwLock::new(std::collections::BTreeMap::new())));

    // 一次性把 middleware 提供的工具注入到 shared_tools。
    // 已存在的同名工具不覆盖（deferred tools 优先保留外部注册版本）。
    {
        let middleware_tools = chain.collect_tools(&cwd);
        let mut tools = shared_tools.write();
        for tool in middleware_tools {
            let arc: Arc<dyn peri_agent::tools::BaseTool> = Arc::from(tool);
            // 使用 insert：有状态工具（如 SubAgentTool）需每 turn 更新。
            tools.insert(arc.name().to_string(), arc);
        }
    }

    // 构造 v2 Session（复用外部 cancel token + 会话级共享 MessageQueue）
    let cwd_arc: Arc<str> = Arc::from(cwd.as_str());
    let frozen_ctx = peri_agent::session::FrozenContext::builder().build();
    let cancel_arc = Arc::new(cancel_token);
    let session = peri_agent::session::Session::new_with_cancel_and_queue(
        cwd_arc,
        frozen_ctx,
        None,
        cancel_arc.clone(),
        shared_queue.clone(),
    );

    // 激活 transcript persistence（compact flags 跨 prompt 持久化）
    if let (Some(store), Some(tid)) = (ctx.thread_store.as_ref(), ctx.thread_id.as_ref()) {
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
    if ctx.session_manager.is_some() {
        if let Some(ref sm) = ctx.session_manager {
            sm.cron_bridge_for(&ctx.session_id);
        }
    } else if let Some(ref scheduler) = cron_scheduler {
        // ── 原 AsyncOwners 块原样保留（912-960 的 { ... } 内容，含 per-turn
        //    SessionInbox + subscribe + bridge task + CronOwner + set_async_owners）──
        {
            let shared_queue_arc = Arc::new(shared_queue.clone());
            let session_inbox = SessionInbox::new(shared_queue_arc);
            let inbox_handle = session_inbox.handle();

            let mut trigger_rx = {
                let mut sched = scheduler.lock();
                sched.subscribe()
            };

            let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
            let shutdown = cancel_arc.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => {
                            tracing::debug!("cron-bridge: shutdown");
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

    // 主 agent 事件侧身份（C2）：peri_agent::agent::stages::StageContext agent_id 与 SubAgentTool 共享 cell
    // 必须同一值——subagent 补发的 SubagentStart.agent_id 指回主 agent。
    let main_agent_id = AgentId::new();

    // 构造 peri_agent::agent::stages::StageContext
    let mut builder = install_tool_invocation_resolver(
        peri_agent::agent::stages::StageContext::builder(turn, transcript, queue)
            .with_agent_id(main_agent_id)
            .with_llm(react_llm)
            .with_tools(shared_tools),
    )
    .with_middleware_chain(Arc::new(chain))
    .with_event_bus(Arc::new(event_bus))
    .with_session_context(session_context)
    .with_tool_registry_snapshot((*tool_registry_snapshot).clone());

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
        let host = peri_agent::session::subagent::SubagentHost {
            thread_store: thread_persistence.store.clone(),
            task_manager: task_manager.clone(),
            bg_event_sender: Some(bg_event_tx),
            on_bg_complete: on_bg_complete.clone(),
            register_runtime: thread_persistence.register_runtime.clone(),
            deregister_runtime: thread_persistence.deregister_runtime.clone(),
            // SubAgent Langfuse bridge：复用父 agent 的 LangfuseTracer 构造独立
            // LangfuseBridge 实例（采样决策继承自父 agent）。
            langfuse_bridge: langfuse_tracer.as_ref().map(|tracer| {
                let bridge = LangfuseBridge::new(
                    Arc::clone(tracer),
                    ctx.provider.display_name().to_string(),
                    None,
                );
                Arc::new(bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>
            }),
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

    // 注入 compact plugin hook 回调
    if !hook_groups_flat.is_empty() {
        {
            let hooks = hook_groups_flat.clone();
            let h_cwd = cwd.clone();
            let h_sid = hook_session_id.clone();
            let h_model = hook_model.clone();
            builder = builder.with_compact_pre_hook(Arc::new(move || {
                let hooks = hooks.clone();
                let cwd = h_cwd.clone();
                let sid = h_sid.clone();
                let model = h_model.clone();
                tokio::spawn(async move {
                    peri_middlewares::hooks::stage_firing::fire_pre_compact(
                        &hooks, &cwd, &sid, "", &model, 0,
                    )
                    .await;
                });
            }));
        }
        {
            let hooks = hook_groups_flat.clone();
            let h_cwd = cwd.clone();
            let h_sid = hook_session_id.clone();
            let h_model = hook_model.clone();
            builder = builder.with_compact_post_hook(Arc::new(
                move |_compacted: bool, affected_count: usize| {
                    let hooks = hooks.clone();
                    let cwd = h_cwd.clone();
                    let sid = h_sid.clone();
                    let model = h_model.clone();
                    tokio::spawn(async move {
                        peri_middlewares::hooks::stage_firing::fire_post_compact(
                            &hooks,
                            &cwd,
                            &sid,
                            "",
                            &model,
                            affected_count,
                        )
                        .await;
                    });
                },
            ));
        }
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
    fn test_stage_context_builder_installs_execute_extra_tool_resolver() {
        use peri_agent::{
            agent::react::ToolCall,
            session::{FrozenContext, Session},
            tools::BaseTool,
        };
        use serde_json::json;

        struct Stub;
        #[async_trait::async_trait]
        impl BaseTool for Stub {
            fn name(&self) -> &str {
                "Write"
            }
            fn description(&self) -> &str {
                ""
            }
            fn parameters(&self) -> serde_json::Value {
                json!({})
            }
            async fn invoke(
                &self,
                _input: serde_json::Value,
                _ctx: peri_agent::tools::ToolContext<'_>,
            ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
                Ok(String::new())
            }
        }

        let session = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
        let turn = session.start_turn();
        let target: Arc<dyn BaseTool> = Arc::new(Stub);
        let tools = Arc::new(RwLock::new(std::collections::BTreeMap::from([(
            "Write".to_string(),
            Arc::clone(&target),
        )])));
        tools.write().insert(
            peri_middlewares::EXECUTE_EXTRA_TOOL_NAME.to_string(),
            Arc::new(peri_middlewares::tool_search::ExecuteExtraTool::new(
                Arc::clone(&tools),
            )),
        );
        let context = install_tool_invocation_resolver(
            peri_agent::agent::stages::StageContext::builder(
                turn,
                session.transcript(),
                session.queue().clone(),
            )
            .with_tools(tools),
        )
        .build();
        let snapshot = context.runtime.tools.read().clone();
        let invocation = context
            .runtime
            .tool_invocation_resolver
            .resolve(
                &ToolCall::new(
                    "call-1",
                    peri_middlewares::EXECUTE_EXTRA_TOOL_NAME,
                    json!({"tool_name": "Write", "params": {}}),
                ),
                &snapshot,
            )
            .unwrap();

        assert!(Arc::ptr_eq(&invocation.target, &target));
        assert_eq!(invocation.policy_call.name, "Write");
    }
    #[test]
    fn test_v2_context_has_null_llm_by_default() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = peri_agent::session::FrozenContext::builder().build();
        let session = peri_agent::session::Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx = peri_agent::agent::stages::StageContext::builder(
            turn,
            session.transcript(),
            session.queue().clone(),
        )
        .build();
        assert_eq!(ctx.runtime.llm.model_name(), "null");
    }
}
