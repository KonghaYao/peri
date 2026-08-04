//! Shared Agent builder（ACP 和 TUI 共用）
//!
//! 提供 Agent 构建相关结构和 `build_agent()` 构建函数，
//! 组装完整的中间件链并产出 `AgentComponents`（供 v2 builder 消费）。
//!
//! 本模块从 peri-tui/src/app/agent.rs:build_bare_agent() 迁移而来，
//! 删除 TUI 特有依赖（ExecutorEvent channel、map_executor_event），
//! 改为通过 `child_handler_factory` 参数从外部注入。

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use peri_agent::{
    agent::{
        compact_v2::CompactConfig,
        events::{AgentEventHandler, ExecutorEvent},
        token::ContextBudget,
    },
    error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot},
    middleware::chain::MiddlewareChain,
    tools::BaseTool,
};

/// 子 Agent 事件 handler 工厂类型
pub(crate) type ChildHandlerFactory =
    Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>;
/// Register callback: (thread_id, cancel_token, cancel_policy_str) → ()
pub(crate) type RegisterRuntimeFn =
    Arc<dyn Fn(String, peri_agent::agent::AgentCancellationToken, String) + Send + Sync>;
/// Deregister callback: &str (thread_id) → ()
pub(crate) type DeregisterRuntimeFn = Arc<dyn Fn(&str) + Send + Sync>;
/// 后台任务完成回调类型
pub(crate) type OnBgCompleteFn =
    Arc<dyn Fn(&peri_agent::agent::events::BackgroundTaskResult) + Send + Sync>;
/// System prompt 构建器类型
pub type SystemPromptBuilder = Arc<
    dyn Fn(Option<&peri_middlewares::agent_define::AgentOverrides>, &str) -> String + Send + Sync,
>;
use peri_agent::{
    agent::model_bridge::AgentModelBridge,
    interaction::{ChannelBroker, MultiplexBroker, UserInteractionBroker},
};
use peri_middlewares::{
    prelude::*,
    tools::{AskUserTool, TodoItem},
};

use crate::langfuse::bridge::LangfuseBridge;
use crate::langfuse::tracer::LangfuseTracer;
use crate::{
    provider::LlmProvider,
    session::agent_pool::{fingerprint, CachedLlmInstances},
};

// ── 共享 Agent 构建（ACP 和 TUI 共用）─────────────────────────────────────────

/// 会话级冻结数据（session/new 一次性捕获，后续轮次直接复用）。
///
/// 零跨依赖分组：四个字段在 `build_agent` 内部独立使用，
/// 不与其它字段共享 mutable state。详见 CLAUDE.md "Frozen Data Flow"。
pub struct FrozenData {
    /// Frozen CLAUDE.md content (None = read from disk each turn, legacy).
    pub claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content.
    pub claude_local_md: Option<String>,
    /// Frozen skills summary (None = scan each turn).
    pub skill_summary: Option<String>,
    /// Frozen session date in YYYY-MM-DD (None = compute fresh each turn).
    pub date: Option<String>,
}

/// 子 Agent 线程持久化分组（零跨依赖）。
///
/// 全部为 `Option`，`build_agent` 内仅用于 SubAgentMiddleware 的链式 `with_*` 调用，
/// 无跨字段约束。
pub(crate) struct ThreadPersistence {
    /// Thread persistence store for child thread creation (None = non-persistent)
    pub store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    /// Parent thread ID for child thread hierarchy (None = top-level agent)
    pub parent_thread_id: Option<String>,
    /// Register callback: called when a child agent starts executing.
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// Deregister callback: called when a child agent finishes.
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
}

pub(crate) struct AcpAgentOutput {
    pub components: AgentComponents,
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<TodoItem>>,
    /// 后台任务完成事件的独立接收端（不随 executor 生命周期销毁）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
}

/// Agent 装配产物（v2 builder 直接消费，P5.3 抽取）
///
/// `build_agent` 直接组装 `MiddlewareChain` + LLM + system prompt 等字段产出本结构，
/// `build_stage_context` 消费它构造 v2 `StageContext`。
pub struct AgentComponents {
    /// 主 LLM（已通过 `AgentModelBridge` 适配为标准 ReAct 抽象）
    pub llm: Arc<dyn ReactLLM + Send + Sync>,
    /// 中间件链（v2 StageContext 直接复用）
    pub chain: MiddlewareChain,
    /// 共享工具注册表（deferred tools，供 ExecuteExtraTool 代理）
    #[allow(clippy::type_complexity)]
    pub shared_tools: Option<Arc<parking_lot::RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>>,
    /// 错误感知建议注册表
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（工具名 + subagent 类型）
    pub tool_registry_snapshot: Arc<ToolRegistrySnapshot>,
    /// 上下文预算（token 监控）
    pub context_budget: Option<ContextBudget>,
    /// Compact 配置
    pub compact_config: Option<CompactConfig>,
}

/// 构建可复用的 Agent（ACP 和 TUI 共用核心构建逻辑）
///
/// 迁移自 peri-tui/src/app/agent.rs:build_bare_agent()。
/// 中间件链和 builder 配置与原函数完全一致。
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
    goal_controller: Option<Arc<dyn peri_agent::goal::GoalController>>,
    background_registry: Option<Arc<peri_middlewares::subagent::BackgroundTaskRegistry>>,
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
    let cron_scheduler = ctx.cron_scheduler.clone();
    let session_id = Some(ctx.session_id.clone());
    let permission_broker = ctx.broker.clone();
    let plugin_skill_roots = ctx.plugin_skill_roots.clone();
    let plugin_agent_dirs = ctx.plugin_agent_dirs.clone();
    let plugin_loaded = ctx.plugin_loaded.clone();
    let hook_groups = ctx.hook_groups.clone();
    let session_start_source = ctx.session_start_source.clone();
    let mcp_pool = ctx.mcp_pool.clone();
    let channel_state = ctx.channel_state.clone();
    let tool_search_index = ctx.tool_search_index.clone();
    let shared_tools = ctx.shared_tools.clone();
    let lsp_servers = ctx.lsp_servers.clone();
    let workflow_executor = ctx.workflow_executor.clone();
    let workflow_middleware = ctx.workflow_middleware.clone();
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
            template.render(&env, &features, &plugin_agent_dirs, None)
        },
    );

    let provider_for_factory = provider.clone();
    let model_name = provider.model_name().to_string();
    let provider_name = provider.display_name().to_string();

    // 提前提取模型实例（chain 构建完成后才组装 AgentModelBridge，
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
    let auto_classifier: Option<Arc<dyn AutoClassifier>> = Some(Arc::new(LlmAutoClassifier::new(
        auto_classifier_model.clone(),
    )));
    // 构造 permission broker（当 channel_state 存在时用 MultiplexBroker 包装）
    let effective_broker: Arc<dyn UserInteractionBroker> = match (&channel_state, &mcp_pool) {
        (Some(cs), Some(pool)) => {
            let channel_broker = Arc::new(ChannelBroker::new(cs.clone(), pool.clone()));
            Arc::new(MultiplexBroker::new(vec![
                ("tui".to_string(), permission_broker.clone()),
                (
                    "channel".to_string(),
                    channel_broker as Arc<dyn UserInteractionBroker>,
                ),
            ]))
        }
        _ => permission_broker.clone(),
    };

    let hitl = HumanInTheLoopMiddleware::with_shared_mode(
        effective_broker.clone(),
        default_requires_approval,
        permission_mode.clone(),
        auto_classifier,
    );

    // AskUser 工具：使用原始 TUI broker（permission_broker），不使用 MultiplexBroker。
    // ChannelBroker 对 Questions 立即返回空答案，MultiplexBroker 竞速时 Channel 总是先返回，
    // 导致 AskUserQuestion 弹窗被绕过。
    let ask_user_tool = AskUserTool::new(permission_broker.clone());

    // 父工具集（供子 agent 继承）
    let filesystem_middleware = FilesystemMiddleware::new();
    let mut parent_tools: Vec<Box<dyn peri_agent::tools::BaseTool>> =
        FilesystemMiddleware::build_tools(&cwd);
    parent_tools.extend(TerminalMiddleware::build_tools(&cwd));
    parent_tools.extend(WebMiddleware::build_tools());
    if let Some(ref pool) = mcp_pool {
        let mcp_tools = peri_middlewares::mcp::build_tool_bridges(pool);
        for tool in mcp_tools {
            parent_tools.push(tool);
        }
        if pool.has_resources() {
            parent_tools.push(Box::new(peri_middlewares::mcp::McpResourceTool::new(
                Arc::clone(pool),
            )));
        }
    }

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

        let mut llm = AgentModelBridge::from_arc(model);
        if let Some(s) = sid {
            llm = llm.with_session_id(s);
        }
        Box::new(llm)
    });

    // 系统提示构建器
    let frozen_language_for_sub = peri_config.config.language.clone();
    let frozen_date_for_sub = frozen_date.clone();
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
            &[],
            frozen_language_for_sub.as_deref(),
        )
    });

    // Parent message snapshot
    let parent_messages: Arc<RwLock<Vec<peri_agent::messages::BaseMessage>>> =
        Arc::new(RwLock::new(Vec::new()));

    // 后台任务通知通道
    let background_registry = background_registry
        .unwrap_or_else(|| Arc::new(peri_middlewares::BackgroundTaskRegistry::new()));

    // 后台任务完成事件的独立通道（不随 executor 生命周期销毁）
    let (bg_event_tx, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();

    // Workflow 中间件（条件注册）
    // 优先复用 session 级 WorkflowMiddleware（progress_store/registry/runner 跨 turn 存活）。
    // 仅在无 session 级实例时创建临时实例（print 模式等）。
    // 完成通知由 executor.rs 的 session 级 consumer 处理，不再需要 per-turn forwarder。
    let mut wf_adaptor: Option<peri_middlewares::workflow::WorkflowMiddlewareAdaptor> = None;
    if let Some(ref executor) = workflow_executor {
        let wf_mw = if let Some(ref session_mw) = workflow_middleware {
            Arc::clone(session_mw)
        } else {
            let (notification_tx, _) = tokio::sync::broadcast::channel(32);
            Arc::new(peri_middlewares::workflow::WorkflowMiddleware::new(
                Arc::clone(executor),
                &cwd,
                notification_tx,
                None, // per-prompt: 不需要 progress_rx
            ))
        };

        // 通过 WorkflowMiddlewareAdaptor 注册到中间件链。
        // build_stage_context 会调 chain.collect_tools() 把 WorkflowTool
        //（以及其它 middleware 提供的工具）一次性 merge 到 shared_tools。
        wf_adaptor = Some(peri_middlewares::workflow::WorkflowMiddlewareAdaptor::new(
            Arc::clone(&wf_mw),
        ));
    }

    let claude_md_excludes = peri_config
        .config
        .claude_md_excludes
        .clone()
        .unwrap_or_default();

    // SubAgent middleware
    // [TRAP] SubAgent 复用 main agent 在 session/new 时捕获的 frozen CLAUDE.md/Skills，
    // 否则文件中途变更会让 SubAgent 看到不同内容，违反第一优先级不变量。
    // Arc<String> 共享：main agent 这里 clone 一份 String 给 SubAgent 的 Arc，
    // 避免每轮 build_tool 重复 clone 大字符串。
    let sub_frozen_claude_md = frozen_claude_md.as_ref().map(|s| Arc::new(s.clone()));
    let sub_frozen_claude_local_md = frozen_claude_local_md.as_ref().map(|s| Arc::new(s.clone()));
    let sub_frozen_skill_summary = frozen_skill_summary.as_ref().map(|s| Arc::new(s.clone()));
    let mut subagent = SubAgentMiddleware::new(
        parent_tools,
        Some(Arc::clone(&event_handler) as Arc<dyn AgentEventHandler>),
        llm_factory.clone(),
    )
    .with_system_builder(system_builder)
    .with_cancel(cancel.clone())
    .with_parent_messages(parent_messages)
    .with_background_registry(Arc::clone(&background_registry))
    .with_bg_event_sender(bg_event_tx)
    .with_registered_hooks(vec![])
    .with_frozen_data(
        sub_frozen_claude_md,
        sub_frozen_claude_local_md,
        sub_frozen_skill_summary,
        Some(Arc::new(system_prompt_for_sub)),
    );
    if let Some(ref cb) = on_bg_complete {
        subagent = subagent.with_on_bg_complete(Arc::clone(cb));
    }
    if let Some(ts) = thread_store {
        subagent = subagent.with_thread_store(ts);
    }
    if let Some(pti) = parent_thread_id {
        subagent = subagent.with_parent_thread_id(pti);
    }
    if let Some(factory) = child_handler_factory {
        subagent = subagent.with_child_handler_factory(factory);
    }
    if let Some(register) = register_runtime {
        subagent = subagent.with_register_runtime(register);
    }
    if let Some(deregister) = deregister_runtime {
        subagent = subagent.with_deregister_runtime(deregister);
    }

    // SubAgent Langfuse bridge：复用父 agent 的 LangfuseTracer，
    // 构造独立 LangfuseBridge 实例供 SubAgent forwarder 使用。
    // 采样决策继承自父 agent（bridge 内部调用 tracer.on_* 方法时，
    // 各方法已内置 sampling.should_emit() 检查）。
    if let Some(ref tracer) = langfuse_tracer {
        let bridge =
            LangfuseBridge::new(Arc::clone(tracer), ctx.provider.display_name().to_string());
        subagent = subagent.with_langfuse_bridge(
            Arc::new(bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>
        );
    }

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

    // 直接构造 MiddlewareChain。
    // build_stage_context 消费 chain + AgentComponents，
    // 并显式调 chain.collect_tools 把 middleware 提供的工具填充到 shared_tools。
    //
    // 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
    // P1-10: 按功能分组注释，降低读取 70 行围墙式构造的心智负担。
    let mut chain = MiddlewareChain::new();

    // ── 第一组：上下文注入器（system prompt 段落 / agent 定义 / 插件 / skills） ──
    chain.add(Box::new({
        let mut mw = AgentsMdMiddleware::new().with_excludes(claude_md_excludes);
        if let Some(main) = frozen_claude_md {
            mw = mw.with_frozen_content(main, frozen_claude_local_md);
        }
        mw
    }));
    chain.add(Box::new(AgentDefineMiddleware::new()));
    chain.add(Box::new(peri_middlewares::PluginMiddleware::new(
        plugin_loaded,
    )));
    // 构造 SkillsMiddleware：collect_tools 提供统一 skill 协议
    // （SkillTool(skill_name) + DiscoverSkillsTool）；旧 Skill(skill, args)
    // 双协议已按 D3 移除，不再单独注册 SkillToolMiddleware。
    let mut skills_mw = SkillsMiddleware::new().with_plugin_roots(plugin_skill_roots.clone());
    if let Some(summary) = frozen_skill_summary {
        skills_mw = skills_mw.with_frozen_summary(summary);
    }
    chain.add(Box::new(skills_mw));
    chain.add(Box::new(
        SkillPreloadMiddleware::new(preload_skills, &cwd)
            .with_plugin_roots(plugin_skill_roots.clone()),
    ));
    chain.add(Box::new(peri_middlewares::AtMentionMiddleware::new(
        cwd.clone().into(),
    )));
    // 新增：图片附件处理（在 @mention 之后，将 @image <path> 转换为 ContentBlock::Image）
    chain.add(Box::new(peri_middlewares::ImageMiddleware::new()));

    // ── 第二组：文件/终端/Web 工具提供器 ──
    chain.add(Box::new(filesystem_middleware));
    chain.add(Box::new(peri_middlewares::GitAttributionMiddleware::new(
        &model_name,
    )));
    chain.add(Box::new({
        let mut tm = TerminalMiddleware::new();
        tm = tm.with_registry(Arc::clone(&background_registry));
        if let Some(ref cb) = on_bg_complete {
            tm = tm.with_on_bg_complete(Arc::clone(cb));
        }
        tm
    }));
    chain.add(Box::new(WebMiddleware::new()));

    // ── 第三组：Todo / Cron ──
    chain.add(Box::new(TodoMiddleware::new(todo_tx)));
    chain.add(Box::new(CronMiddleware::new(
        cron_scheduler.unwrap_or_else(|| {
            Arc::new(parking_lot::Mutex::new(CronScheduler::new(
                tokio::sync::mpsc::unbounded_channel().0,
            )))
        }),
    )));

    // ── 第四组：Hook 中间件（插件 hooks + 自定义 hooks） ──
    tracing::info!(
        groups = hook_groups.len(),
        total_hooks = hook_groups.iter().map(|g| g.len()).sum::<usize>(),
        session_start = session_start_source.is_some(),
        "Builder: assembling HookMiddleware from groups"
    );
    if !hook_groups.is_empty() {
        let hook_llm_factory: Arc<
            dyn Fn() -> Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> + Send + Sync,
        > = Arc::new({
            let factory = llm_factory.clone();
            move || factory(None)
        });
        for (i, group) in hook_groups.into_iter().enumerate() {
            if group.is_empty() {
                continue;
            }
            let group_size = group.len();
            let mw = peri_middlewares::hooks::HookMiddleware::with_session_start(
                group,
                hook_llm_factory.clone(),
                &cwd,
                "",
                "",
                permission_mode.clone(),
                provider_name.clone(),
                session_start_source.clone(),
            );
            tracing::info!(
                group_index = i,
                group_size,
                "Builder: HookMiddleware group {} created with {} hooks",
                i,
                group_size
            );
            chain.add(Box::new(mw));
        }
    }

    // ── 第五组：HITL + SubAgent（条件中间件） ──
    chain.add(Box::new(hitl));
    chain.add(Box::new(subagent));

    // ── 第六组：MCP / Workflow / ToolSearch（工具提供器） ──
    if let Some(pool) = mcp_pool {
        chain.add(Box::new(peri_middlewares::mcp::McpMiddleware::new(pool)));
    }

    // Workflow 中间件（通过 collect_tools 注册 WorkflowTool 为 deferred tool）
    if let Some(adaptor) = wf_adaptor {
        chain.add(Box::new(adaptor));
    }

    // ToolSearch 中间件
    chain.add(Box::new(peri_middlewares::ToolSearchMiddleware::new(
        Arc::clone(&tool_search_index),
        Arc::clone(&shared_tools),
    )));

    // AskUserTool：v1 通过 register_tool 注册到 executor.self.tools（每轮 execute 合并）。
    // v2 stages 不调 execute()，改为一次性 insert 到 shared_tools。
    // build_stage_context 随后调 chain.collect_tools merge 时，本工具已存在不会覆盖。
    {
        let mut tools = shared_tools.write();
        tools.insert("AskUserQuestion".to_string(), Arc::new(ask_user_tool));
    }

    // 错误感知建议：从 shared_tools 构造 snapshot（所有工具都已注册）
    let all_tool_names: Vec<String> = shared_tools.read().keys().cloned().collect();
    let agents_dir = std::path::Path::new(&cwd).join(".claude").join("agents");
    let agents_dir_opt = if agents_dir.exists() {
        Some(agents_dir.as_path())
    } else {
        None
    };
    let snapshot = peri_middlewares::error_suggest::build_tool_registry_snapshot(
        all_tool_names,
        agents_dir_opt,
    );
    let registry = peri_middlewares::error_suggest::build_default_registry();

    // ── 第七组：LSP / ErrorSuggest（辅助诊断中间件） ──
    if !lsp_servers.is_empty() {
        let lsp_config = peri_lsp::config::LspConfigFile {
            lsp_servers: lsp_servers
                .into_iter()
                .map(|s| (s.name.clone(), s))
                .collect(),
        };
        tracing::info!(
            target: "lsp",
            servers = lsp_config.lsp_servers.len(),
            "LSP 中间件已注册"
        );
        chain.add(Box::new(peri_middlewares::LspMiddleware::new(
            cwd.clone(),
            lsp_config,
        )));
    }

    // [v2] CompactMiddleware 已移除——自动 compact 由 v2 stages/compact.rs 接管
    // （run_react_loop 在每轮开头调用 compact_v2::run_compact）。
    // 详见 CLAUDE.md「v2 单路径架构」+ stages/compact.rs。
    let auxiliary_model_for_cache: Option<Arc<dyn peri_model::Model>> = mw_auxiliary_model.clone();

    // GoalMiddleware（链最后）
    // goal active 时注入递增紧迫感 steering + 设 block_continue 让 agent 自驱续跑
    if let Some(controller) = &goal_controller {
        let goal_mw = peri_middlewares::GoalMiddleware::new(
            Arc::clone(controller),
            auxiliary_model_for_cache.clone(),
        );
        chain.add(Box::new(goal_mw));
    }

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
        error_suggest_registry: Some(registry),
        tool_registry_snapshot: Arc::new(snapshot),
        context_budget: Some(context_budget),
        compact_config: Some(compact_config),
    };

    (
        AcpAgentOutput {
            components,
            todo_rx,
            bg_event_rx,
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
// 本函数不再挂载 turn 级 CronOwner。
//
// 仅 print 模式（-p，无 SessionManager）走本函数的 turn 级挂载：
// 1. 创建 SessionInbox（await-wake wrapper around shared_queue）。
// 2. 从 CronScheduler 订阅 trigger_rx（通过 subscribe()）。
// 3. 启动 CronTrigger→String 桥接任务。
// 4. 创建并启动 CronOwner（trigger_rx → inbox）。
// 5. 通过 Session::set_async_owners 注入到 Session。

// Note: 以下类型已在文件头部导入，此处仅补充增量导入。
use peri_agent::{
    agent::{
        events_v2::{EventBus, EventBusConfig, EventHandles},
        react::ReactLLM,
        session::{cron_owner::CronOwner, inbox::SessionInbox},
        stages::{SharedToolMap, StageContext, StageContextBuilder},
    },
    group::pipeline::AgentId,
    session::Session as V2Session,
};

/// 为 ACP 生产 StageContext 安装 wrapper-aware canonical invocation resolver。
fn install_tool_invocation_resolver(builder: StageContextBuilder) -> StageContextBuilder {
    builder.with_tool_invocation_resolver(Arc::new(
        peri_middlewares::ExecuteExtraToolResolver::default(),
    ))
}

/// v2 builder 产物
pub(crate) struct V2AgentOutput {
    /// 已配置的 StageContext（用于 run_react_loop）
    pub context: StageContext,
    /// v2 Session（持有 transcript + queue + store）
    pub session: Arc<V2Session>,
    /// EventBus 消费端（转 ExecutorEvent 用）
    pub event_handles: EventHandles,
    /// Todo 更新通道（spawn todo forwarder 用）
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<peri_middlewares::tools::TodoItem>>,
    /// 后台任务完成事件接收端（spawn bg event pump 用）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
}

/// 从 SessionContext 构造 StageContext
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
    goal_controller: Option<Arc<dyn peri_agent::goal::GoalController>>,
    background_registry: Option<Arc<peri_middlewares::subagent::BackgroundTaskRegistry>>,
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

    // 提取 cron_scheduler
    let cron_scheduler = ctx.cron_scheduler.clone();

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
        let probe_bg = background_registry.clone();
        probe_bg.map(|reg| {
            Arc::new(move || reg.active_count() > 0) as Arc<dyn Fn() -> bool + Send + Sync>
        })
    };

    // 调用 build_agent 构造完整 agent（含中间件链 + LLM）
    let (agent_output, new_cached) = build_agent(
        ctx,
        system_prompt,
        subagent_system_prompt,
        frozen,
        event_handler,
        agent_overrides,
        preload_skills,
        child_handler_factory,
        auxiliary_model,
        thread_persistence,
        goal_controller,
        background_registry,
        on_bg_complete,
        cached_llm,
        langfuse_tracer,
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
        ..
    } = agent_output.components;

    let shared_tools: SharedToolMap = shared_tools_opt
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
    let frozen = peri_agent::session::FrozenContext::builder().build();
    let cancel_arc = Arc::new(cancel_token);
    let session = V2Session::new_with_cancel_and_queue(
        cwd_arc,
        frozen,
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

    // 构造 StageContext
    let mut builder = install_tool_invocation_resolver(
        StageContext::builder(turn, transcript, queue)
            .with_agent_id(AgentId::new())
            .with_llm(react_llm)
            .with_tools(shared_tools),
    )
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
            StageContext::builder(turn, session.transcript(), session.queue().clone())
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
        let session = V2Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx =
            StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
        assert_eq!(ctx.runtime.llm.model_name(), "null");
    }
}
