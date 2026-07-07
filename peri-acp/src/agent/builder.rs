//! Shared Agent builder（ACP 和 TUI 共用）
//!
//! 提供 `AcpAgentConfig` 配置结构和 `build_agent()` 构建函数，
//! 组装完整的中间件链并产出 `AgentComponents`（供 v2 builder 消费）。
//!
//! 本模块从 peri-tui/src/app/agent.rs:build_bare_agent() 迁移而来，
//! 删除 TUI 特有依赖（ExecutorEvent channel、map_executor_event），
//! 改为通过 `child_handler_factory` 参数从外部注入。

use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use peri_agent::{
    agent::{
        compact::CompactConfig,
        events::{AgentEventHandler, ExecutorEvent},
        token::ContextBudget,
    },
    error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot},
    llm::BaseModel,
    middleware::chain::MiddlewareChain,
    tools::BaseTool,
};

/// 子 Agent 事件 handler 工厂类型
pub type ChildHandlerFactory = Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>;
/// Register callback: (thread_id, cancel_token, cancel_policy_str) → ()
pub type RegisterRuntimeFn =
    Arc<dyn Fn(String, peri_agent::agent::AgentCancellationToken, String) + Send + Sync>;
/// Deregister callback: &str (thread_id) → ()
pub type DeregisterRuntimeFn = Arc<dyn Fn(&str) + Send + Sync>;
/// System prompt 构建器类型
pub type SystemPromptBuilder = Arc<
    dyn Fn(Option<&peri_middlewares::agent_define::AgentOverrides>, &str) -> String + Send + Sync,
>;
use peri_agent::{
    agent::AgentCancellationToken,
    interaction::{ChannelBroker, ChannelState, MultiplexBroker, UserInteractionBroker},
    llm::BaseModelReactLLM,
};
use peri_middlewares::{
    prelude::*,
    skills::SkillRoot,
    tools::{AskUserTool, TodoItem},
};

use crate::{
    provider::{config::PeriConfig, LlmProvider},
    session::agent_pool::{fingerprint, AgentPool, CachedLlmInstances},
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

/// Auxiliary LLM 模型（compact 摘要生成、goal steering 等场景复用主 LLM）
///
/// [v2] CompactMiddleware 已删除，但 auxiliary_model 仍由 v2 stages/compact.rs、
/// GoalMiddleware 和 CachedLlmInstances 复用，因此保留此字段。
pub struct AuxiliaryModel {
    pub model: Option<Arc<dyn BaseModel>>,
}

/// 子 Agent 线程持久化分组（零跨依赖）。
///
/// 全部为 `Option`，`build_agent` 内仅用于 SubAgentMiddleware 的链式 `with_*` 调用，
/// 无跨字段约束。
pub struct ThreadPersistence {
    /// Thread persistence store for child thread creation (None = non-persistent)
    pub store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    /// Parent thread ID for child thread hierarchy (None = top-level agent)
    pub parent_thread_id: Option<String>,
    /// Register callback: called when a child agent starts executing.
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// Deregister callback: called when a child agent finishes.
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
}

/// 共享 Agent 构建配置（ACP 和 TUI 共用）
///
/// **结构稳定性**：中间件添加顺序是 `[TRAP]` 守护契约，禁止重排。
/// 本结构仅做字段分组，`build_agent` 函数体保持单体。
pub struct AcpAgentConfig {
    pub provider: LlmProvider,
    pub cwd: String,
    pub system_prompt: String,
    /// Frozen 会话数据（FrozenData 分组，零跨依赖）
    pub frozen: FrozenData,
    pub event_handler: Arc<dyn AgentEventHandler>,
    pub cancel: AgentCancellationToken,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub peri_config: Arc<PeriConfig>,
    pub cron_scheduler: Option<Arc<parking_lot::Mutex<CronScheduler>>>,
    pub agent_overrides: Option<peri_middlewares::agent_define::AgentOverrides>,
    pub preload_skills: Vec<String>,
    pub session_id: Option<String>,
    pub broker: Arc<dyn UserInteractionBroker>,
    pub plugin_skill_roots: Vec<SkillRoot>,
    pub plugin_agent_dirs: Vec<std::path::PathBuf>,
    pub hook_groups: Vec<Vec<RegisteredHook>>,
    pub session_start_source: Option<String>,
    pub mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    /// Channel 共享状态（None = 不启用 channel 功能，不使用 MultiplexBroker）
    pub channel_state: Option<Arc<ChannelState>>,
    pub tool_search_index: Arc<peri_middlewares::tool_search::ToolSearchIndex>,
    pub shared_tools: Arc<RwLock<HashMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
    /// 子 Agent 专用事件 handler factory（由调用方提供，取代 TUI 的 child_event_tx）
    pub child_handler_factory: Option<ChildHandlerFactory>,
    /// LSP 服务器配置（由调用方从 settings.json + 插件配置组装）
    pub lsp_servers: Vec<peri_lsp::config::LspServerConfig>,
    /// Auxiliary LLM 模型（v2 stages compact + goal middleware 复用）
    pub auxiliary_model: Option<Arc<dyn BaseModel>>,
    /// 子 Agent 线程持久化分组（ThreadPersistence 分组，零跨依赖）
    pub thread_persistence: ThreadPersistence,
    /// Goal controller（None = 不启用 goal 功能）
    pub goal_controller: Option<Arc<dyn peri_agent::goal::GoalController>>,
    /// Workflow agent 执行器（None = 不启用 workflow 功能）
    pub workflow_executor: Option<Arc<dyn peri_workflow::runner::AgentExecutor>>,
    /// Session 级 WorkflowMiddleware（None = 每轮创建临时实例）。
    pub workflow_middleware: Option<Arc<peri_middlewares::workflow::WorkflowMiddleware>>,
    /// Session 级 BackgroundTaskRegistry（跨 prompt 存活，取代 per-prompt 创建）
    pub background_registry: Option<Arc<peri_middlewares::subagent::BackgroundTaskRegistry>>,
}

pub struct AcpAgentOutput {
    pub components: AgentComponents,
    pub todo_rx: tokio::sync::mpsc::Receiver<Vec<TodoItem>>,
    /// 后台任务完成事件的独立接收端（不随 executor 生命周期销毁）
    pub bg_event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
}

/// Agent 装配产物（v2 builder 直接消费，P5.3 抽取）
///
/// `build_agent` 直接组装 `MiddlewareChain` + LLM + system prompt 等字段产出本结构，
/// `builder_v2::build_stage_context` 消费它构造 v2 `StageContext`。
pub struct AgentComponents {
    /// 主 LLM（已包装 RetryableLLM）
    pub llm: peri_agent::llm::RetryableLLM<BaseModelReactLLM>,
    /// 中间件链（v2 StageContext 直接复用）
    pub chain: MiddlewareChain,
    /// 共享工具注册表（deferred tools，供 ExecuteExtraTool 代理）
    #[allow(clippy::type_complexity)]
    pub shared_tools: Option<Arc<parking_lot::RwLock<HashMap<String, Arc<dyn BaseTool>>>>>,
    /// 错误感知建议注册表
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（工具名 + subagent 类型）
    pub tool_registry_snapshot: Arc<ToolRegistrySnapshot>,
    /// Frozen system prompt
    pub system_prompt: Option<String>,
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
/// `pool` 提供 SubAgent LLM 缓存，跨 SubAgent 调用复用 `Arc<dyn BaseModel>`
/// （含共享的 `reqwest::Client`）。首次同模型 SubAgent 调用时创建新实例并插入缓存，
/// 后续调用直接命中缓存，避免每 SubAgent 分配 ~1-2 MB 的 HTTP client。
pub fn build_agent(
    cfg: AcpAgentConfig,
    cached_llm: Option<&CachedLlmInstances>,
    pool: &Arc<parking_lot::Mutex<AgentPool>>,
) -> (AcpAgentOutput, Option<CachedLlmInstances>) {
    // destructure background_registry from config at the top
    let AcpAgentConfig {
        provider,
        cwd,
        system_prompt,
        frozen:
            FrozenData {
                claude_md: frozen_claude_md,
                claude_local_md: frozen_claude_local_md,
                skill_summary: frozen_skill_summary,
                date: frozen_date,
            },
        event_handler,
        cancel,
        permission_mode,
        peri_config,
        cron_scheduler,
        agent_overrides,
        preload_skills,
        session_id,
        broker: permission_broker,
        plugin_skill_roots,
        plugin_agent_dirs,
        hook_groups,
        session_start_source,
        mcp_pool,
        channel_state,
        tool_search_index,
        shared_tools,
        child_handler_factory,
        lsp_servers,
        auxiliary_model: mw_auxiliary_model,
        thread_persistence:
            ThreadPersistence {
                store: thread_store,
                parent_thread_id,
                register_runtime,
                deregister_runtime,
            },
        goal_controller,
        workflow_executor,
        workflow_middleware,
        background_registry,
    } = cfg;

    // 应用 agent overrides 到系统提示词
    let system_prompt = agent_overrides.as_ref().map_or_else(
        || system_prompt.clone(),
        |ov| {
            let features = crate::prompt::PromptFeatures::detect();
            crate::prompt::build_system_prompt(
                Some(ov),
                &cwd,
                features,
                &plugin_agent_dirs,
                None,
                None,
            )
        },
    );

    let provider_for_factory = provider.clone();
    let model_name = provider.model_name().to_string();
    let provider_name = provider.display_name().to_string();

    // 提前提取 BaseModel（chain 构建完成后才组装 RetryableLLM，
    // 以便收集中间件 prompt_contribution 合并到 system prompt）。
    let base_model: Box<dyn BaseModel> = provider.into_model();
    let context_window_raw = base_model.context_window();

    // Todo channel
    let (todo_tx, todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);

    // HITL middleware — reuse auto_classifier model from cache when available
    let auto_classifier_model: Arc<tokio::sync::Mutex<Box<dyn BaseModel>>> = cached_llm
        .map(|c| c.auto_classifier_model.clone())
        .unwrap_or_else(|| {
            Arc::new(tokio::sync::Mutex::new(
                provider_for_factory.clone().into_model(),
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
        let model: Arc<dyn BaseModel> =
            crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(
                &pool_for_subagent,
                &fp,
                || match &p {
                    Some(provider) => provider.clone().into_model(),
                    None => provider_clone.clone().into_model(),
                },
            );

        let mut llm = BaseModelReactLLM::from_arc(model);
        if let Some(s) = sid {
            llm = llm.with_session_id(s);
        }
        Box::new(peri_agent::llm::RetryableLLM::new(
            llm,
            peri_agent::llm::RetryConfig::default(),
        ))
    });

    // 系统提示构建器
    let frozen_language_for_sub = peri_config.config.language.clone();
    let frozen_date_for_sub = frozen_date.clone();
    let system_builder: SystemPromptBuilder = Arc::new(move |overrides, cwd_dir| {
        let features = crate::prompt::PromptFeatures::detect();
        crate::prompt::build_system_prompt(
            overrides,
            cwd_dir,
            features,
            &[],
            frozen_date_for_sub.as_deref(),
            frozen_language_for_sub.as_deref(),
        )
    });

    // Parent message snapshot
    let parent_messages: Arc<RwLock<Vec<peri_agent::messages::BaseMessage>>> =
        Arc::new(RwLock::new(Vec::new()));

    // 后台任务通知通道
    let background_registry = background_registry.unwrap_or_else(|| {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(peri_middlewares::BackgroundTaskRegistry::new(tx))
    });

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
            ))
        };

        // 通过 WorkflowMiddlewareAdaptor 注册到中间件链。
        // builder_v2::build_stage_context 会调 chain.collect_tools() 把 WorkflowTool
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
    );
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

    // 上下文预算
    let mut context_window = context_window_raw;
    let context_1m = peri_config.config.context_1m.unwrap_or(false);
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
    // builder_v2::build_stage_context 消费 chain + AgentComponents，
    // 并显式调 chain.collect_tools 把 middleware 提供的工具填充到 shared_tools。
    //
    // 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new({
        let mut mw = AgentsMdMiddleware::new().with_excludes(claude_md_excludes);
        if let Some(main) = frozen_claude_md {
            mw = mw.with_frozen_content(main, frozen_claude_local_md);
        }
        mw
    }));
    chain.add(Box::new(AgentDefineMiddleware::new()));
    chain.add(Box::new({
        let mut mw = SkillsMiddleware::new().with_plugin_roots(plugin_skill_roots.clone());
        if let Some(summary) = frozen_skill_summary {
            mw = mw.with_frozen_summary(summary);
        }
        mw
    }));
    chain.add(Box::new(
        SkillPreloadMiddleware::new(preload_skills, &cwd)
            .with_plugin_roots(plugin_skill_roots.clone()),
    ));
    chain.add(Box::new(peri_middlewares::AtMentionMiddleware::new(
        cwd.clone().into(),
    )));
    chain.add(Box::new(filesystem_middleware));
    chain.add(Box::new(peri_middlewares::GitAttributionMiddleware::new(
        &model_name,
    )));
    chain.add(Box::new({
        let mut tm = TerminalMiddleware::new();
        tm = tm.with_registry(Arc::clone(&background_registry));
        tm
    }));
    chain.add(Box::new(WebMiddleware::new()));
    chain.add(Box::new(TodoMiddleware::new(todo_tx)));
    chain.add(Box::new(CronMiddleware::new(
        cron_scheduler.unwrap_or_else(|| {
            Arc::new(parking_lot::Mutex::new(CronScheduler::new(
                tokio::sync::mpsc::unbounded_channel().0,
            )))
        }),
    )));

    // Hook middleware groups
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

    chain.add(Box::new(hitl));
    chain.add(Box::new(subagent));

    // MCP 中间件
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
    // builder_v2 随后调 chain.collect_tools merge 时，本工具已存在不会覆盖。
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

    // LSP 中间件（条件注册，当有 LSP 服务器配置时）
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
    let auxiliary_model_for_cache: Option<Arc<dyn BaseModel>> = mw_auxiliary_model.clone();

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

    // 构造 BaseModelReactLLM（带系统提示词）
    let merged_for_storage = merged_system_prompt.clone();
    let mut base_llm =
        peri_agent::llm::BaseModelReactLLM::new(base_model).with_system(merged_system_prompt);
    if let Some(ref sid) = session_id {
        base_llm = base_llm.with_session_id(sid);
    }
    let model =
        peri_agent::llm::RetryableLLM::new(base_llm, peri_agent::llm::RetryConfig::default())
            .with_event_handler(Arc::clone(&event_handler));

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
        system_prompt: Some(merged_for_storage),
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
