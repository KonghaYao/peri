//! 生产中间件链装配（ARC-MIDDLEWARE-001）。
//!
//! 3.0 归位（L2）：链装配实现自 `peri-acp/src/agent/builder.rs` 迁入本模块。
//! 链序事实源位于 Agent 层 session 工厂
//! （`peri-agent/src/session/factory.rs` 的 `production_blueprint`），
//! 本模块按蓝本构造中间件实例——顺序是行为契约，禁止重排。
//!
//! 依赖方向说明：装配实现引用具体中间件类型（本 crate），
//! 经 Agent 层 [`MiddlewareChainAssembler`] trait 边界接入，
//! 避免 Agent 层反向依赖本 crate 成环；依赖反转（中间件类型下沉）
//! 完成后装配实现整体物理迁入 Agent 层。

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use peri_agent::{
    agent::{
        async_tasks::{BgTaskKind, TaskManager},
        events::{AgentEventHandler, BackgroundTaskResult, ExecutorEvent},
        react::ReactLLM,
        AgentCancellationToken, LangfuseBridgeLike,
    },
    error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot},
    goal::GoalController,
    interaction::{ChannelBroker, MultiplexBroker, UserInteractionBroker},
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    session::factory::{
        ChainSlot, ChildHandlerFactory, DeregisterRuntimeFn, MiddlewareChainAssembler,
        RegisterRuntimeFn,
    },
    thread::ThreadStore,
    tools::BaseTool,
};
use peri_resources::lsp::config::{LspConfigFile, LspServerConfig};
use peri_workflow::runner::AgentExecutor;

use crate::{
    agent_define::AgentOverrides,
    cron::{CronMiddleware, CronScheduler},
    error_suggest,
    hitl::{
        default_requires_approval, AutoClassifier, HumanInTheLoopMiddleware, LlmAutoClassifier,
        SharedPermissionMode,
    },
    hooks::{HookMiddleware, RegisteredHook},
    mcp::{build_tool_bridges, McpClientPool, McpMiddleware, McpResourceTool},
    middleware::{FilesystemMiddleware, TerminalMiddleware, TodoMiddleware, WebMiddleware},
    plugin::{LoadedPlugin, PluginMiddleware},
    skills::{SkillRoot, SkillsMiddleware},
    subagent::{SkillPreloadMiddleware, SubAgentMiddleware},
    tool_search::{ToolSearchIndex, ToolSearchMiddleware},
    tools::{AskUserTool, TodoItem},
    workflow::{WorkflowMiddleware, WorkflowMiddlewareAdaptor},
    AgentDefineMiddleware, AgentsMdMiddleware, AtMentionMiddleware, GitAttributionMiddleware,
    GoalMiddleware, ImageMiddleware, LspMiddleware,
};

/// 后台任务完成回调类型（第二参为任务 kind，供 continuation scheduler 过滤）
pub type OnBgCompleteFn = Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>;
/// System prompt 构建器类型
pub type SystemPromptBuilder = Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>;

/// 链装配上下文（Agent 层装配接口的上下文投影）。
///
/// 由 ACP 侧（`peri-acp/src/agent/builder.rs`）从 `SessionContext` 投影构造，
/// 仅含中间件构造所需的依赖；LLM/prompt 渲染等 ACP 私有逻辑不进入本结构。
#[allow(clippy::type_complexity)]
pub struct AssemblyContext {
    // ── 会话级 ──
    /// 工作目录
    pub cwd: String,
    /// 取消令牌（子 agent / 工具执行共享）
    pub cancel: AgentCancellationToken,
    /// 用户交互 broker（HITL 审批）
    pub broker: Arc<dyn UserInteractionBroker>,
    /// 权限模式
    pub permission_mode: Arc<SharedPermissionMode>,
    // ── 模型 ──
    /// 模型名称（GitAttribution 注入用）
    pub model_name: String,
    /// 模型显示名（hook 注入用）
    pub provider_name: String,
    /// 辅助模型（goal steering / compact）
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    /// 自动分类模型（HITL auto-classifier）
    pub auto_classifier_model: Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>>,
    // ── 配置 / 插件 / 技能 ──
    /// CLAUDE.md 排除项
    pub claude_md_excludes: Vec<String>,
    /// 预加载技能名
    pub preload_skills: Vec<String>,
    /// 插件技能根目录
    pub plugin_skill_roots: Vec<SkillRoot>,
    /// 已加载插件
    pub plugin_loaded: Vec<LoadedPlugin>,
    /// Hook 组（每组一个 HookMiddleware 实例）
    pub hook_groups: Vec<Vec<RegisteredHook>>,
    /// session 启动来源（hook 注入用）
    pub session_start_source: Option<String>,
    // ── 外部服务 ──
    /// Cron 调度器（None = 构造临时实例）
    pub cron_scheduler: Option<Arc<parking_lot::Mutex<CronScheduler>>>,
    /// MCP 连接池（None = 不注册 MCP 中间件/工具）
    pub mcp_pool: Option<Arc<McpClientPool>>,
    /// Channel 状态（MultiplexBroker 包装用）
    pub channel_state: Option<Arc<peri_agent::interaction::ChannelState>>,
    /// 工具搜索索引
    pub tool_search_index: Arc<ToolSearchIndex>,
    /// 共享工具注册表（deferred tools；AskUserTool 插入、snapshot 构造）
    pub shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    /// LSP server 配置（非空时注册 LspMiddleware）
    pub lsp_servers: Vec<LspServerConfig>,
    /// Workflow executor（Some 时注册 Workflow 中间件）
    pub workflow_executor: Option<Arc<dyn AgentExecutor>>,
    /// 会话级 WorkflowMiddleware（复用，None = 构造临时实例）
    pub workflow_middleware: Option<Arc<WorkflowMiddleware>>,
    // ── 事件 / 后台 ──
    /// 事件 handler（子 agent 事件转发）
    pub event_handler: Arc<dyn AgentEventHandler>,
    /// 后台任务注册表（session 级，None 时上层已回退为临时实例）
    pub task_manager: Arc<TaskManager>,
    /// 后台任务完成事件发送端（bg_event_rx 由上层持有）
    pub bg_event_tx: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    /// 后台任务完成回调
    pub on_bg_complete: Option<OnBgCompleteFn>,
    /// SubAgent Langfuse bridge（由上层构造注入）
    pub langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    // ── 子 agent 持久化 ──
    /// 子线程持久化存储
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 父线程 ID（子 agent 层级）
    pub parent_thread_id: Option<String>,
    /// 子 agent 启动注册回调
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// 子 agent 事件 handler 工厂
    pub child_handler_factory: Option<ChildHandlerFactory>,
    // ── 冻结数据 / prompt ──
    /// 冻结 CLAUDE.md（None = 每轮从磁盘读，legacy）
    pub frozen_claude_md: Option<String>,
    /// 冻结 CLAUDE.local.md
    pub frozen_claude_local_md: Option<String>,
    /// 冻结 skills 摘要
    pub frozen_skill_summary: Option<String>,
    /// 子 agent / fork 复用的冻结 prompt（无 16_workflow 版本）
    pub system_prompt_for_sub: String,
    // ── 工厂 ──
    /// 子 agent LLM 工厂（支持 SubAgent LLM 缓存复用）
    pub llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    /// System prompt 构建器（SubAgent 用）
    pub system_builder: SystemPromptBuilder,
    /// Todo 更新通道发送端（todo_rx 由上层持有）
    pub todo_tx: tokio::sync::mpsc::Sender<Vec<TodoItem>>,
    // ── goal ──
    /// Goal 控制器（Some 时在链最后注册 GoalMiddleware）
    pub goal_controller: Option<Arc<dyn GoalController>>,
}

/// 链装配产物（ACP `build_agent` 直接消费）
pub struct ChainAssembly {
    /// 中间件链（StageContext 复用）
    pub chain: MiddlewareChain,
    /// SubAgent 中间件原实例（链中已有一份 clone；供上层注入主 agent 身份）
    pub subagent_mw: Option<SubAgentMiddleware>,
    /// 错误感知建议注册表
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（工具名 + subagent 类型）
    pub tool_registry_snapshot: Arc<ToolRegistrySnapshot>,
}

/// 生产链装配器（当前唯一装配实现，见模块文档）。
pub struct ProductionChainAssembler;

impl MiddlewareChainAssembler for ProductionChainAssembler {
    type Context = AssemblyContext;
    type Output = ChainAssembly;

    /// 按 Agent 层 `production_blueprint` 的槽位顺序构造中间件链。
    ///
    /// 链序由蓝本保证（ARC-MIDDLEWARE-001 事实源在 Agent 层工厂）；
    /// 本实现只负责逐槽位构造实例，条件注册（MCP/Workflow/LSP/Goal）
    /// 与 Hook 组展开按上下文判断，行为与迁移前
    /// `peri-acp/src/agent/builder.rs` 完全一致。
    fn assemble(&self, blueprint: &[ChainSlot], ctx: &Self::Context) -> Self::Output {
        let AssemblyContext {
            cwd,
            cancel,
            broker,
            permission_mode,
            model_name,
            provider_name,
            auxiliary_model,
            auto_classifier_model,
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
            shared_tools,
            lsp_servers,
            workflow_executor,
            workflow_middleware,
            event_handler,
            task_manager,
            bg_event_tx: _,
            on_bg_complete,
            langfuse_bridge: _,
            thread_store: _,
            parent_thread_id: _,
            register_runtime: _,
            deregister_runtime: _,
            child_handler_factory,
            frozen_claude_md,
            frozen_claude_local_md,
            frozen_skill_summary,
            system_prompt_for_sub: _,
            llm_factory,
            system_builder,
            todo_tx,
            goal_controller,
        } = ctx;

        // HITL middleware — reuse auto_classifier model from cache when available
        let auto_classifier: Option<Arc<dyn AutoClassifier>> = Some(Arc::new(
            LlmAutoClassifier::new(auto_classifier_model.clone()),
        ));
        // 构造 permission broker（当 channel_state 存在时用 MultiplexBroker 包装）
        let effective_broker: Arc<dyn UserInteractionBroker> = match (channel_state, mcp_pool) {
            (Some(cs), Some(pool)) => {
                let channel_broker = Arc::new(ChannelBroker::new(cs.clone(), pool.clone()));
                Arc::new(MultiplexBroker::new(vec![
                    ("tui".to_string(), broker.clone()),
                    (
                        "channel".to_string(),
                        channel_broker as Arc<dyn UserInteractionBroker>,
                    ),
                ]))
            }
            _ => broker.clone(),
        };

        // AskUser 工具：使用原始 broker，不使用 MultiplexBroker。
        // ChannelBroker 对 Questions 立即返回空答案，MultiplexBroker 竞速时 Channel 总是先返回，
        // 导致 AskUserQuestion 弹窗被绕过。
        let ask_user_tool = AskUserTool::new(broker.clone());

        // 父工具集（供子 agent 继承）
        let mut parent_tools: Vec<Box<dyn BaseTool>> = FilesystemMiddleware::build_tools(cwd);
        parent_tools.extend(TerminalMiddleware::build_tools(cwd));
        parent_tools.extend(WebMiddleware::build_tools());
        if let Some(ref pool) = mcp_pool {
            let mcp_tools = build_tool_bridges(pool);
            for tool in mcp_tools {
                parent_tools.push(tool);
            }
            if pool.has_resources() {
                parent_tools.push(Box::new(McpResourceTool::new(Arc::clone(pool))));
            }
        }

        // Workflow 中间件（条件注册）
        // 优先复用 session 级 WorkflowMiddleware（progress_store/registry/runner 跨 turn 存活）。
        // 仅在无 session 级实例时创建临时实例（print 模式等）。
        let mut wf_adaptor: Option<WorkflowMiddlewareAdaptor> = None;
        if let Some(ref executor) = workflow_executor {
            let wf_mw = if let Some(ref session_mw) = workflow_middleware {
                Arc::clone(session_mw)
            } else {
                let (notification_tx, _) = tokio::sync::broadcast::channel(32);
                Arc::new(WorkflowMiddleware::new(
                    Arc::clone(executor),
                    cwd,
                    notification_tx,
                    None, // per-prompt: 不需要 progress_rx
                ))
            };

            // 通过 WorkflowMiddlewareAdaptor 注册到中间件链。
            // 上层会调 chain.collect_tools() 把 WorkflowTool
            //（以及其它 middleware 提供的工具）一次性 merge 到 shared_tools。
            wf_adaptor = Some(WorkflowMiddlewareAdaptor::new(Arc::clone(&wf_mw)));
        }

        // SubAgent middleware（L3 瘦身：只声明工具与发起意图）。
        // [TRAP] SubAgent 复用 main agent 在 session/new 时捕获的 frozen CLAUDE.md/Skills
        // （L3 起由 Agent 层 spawn_subagent 从父 session copy，此处不再透传）；
        // 运行时通道（thread_store / task_manager / bg_event_sender / register /
        // deregister / langfuse_bridge / frozen 回退）统一经 SubagentHost 注入
        // 主 session（builder 侧构造），此处只留工具声明字段。
        let mut subagent = SubAgentMiddleware::new(
            parent_tools,
            Some(Arc::clone(event_handler) as Arc<dyn AgentEventHandler>),
            llm_factory.clone(),
        )
        .with_system_builder(system_builder.clone())
        .with_cancel(cancel.clone())
        .with_parent_messages(Arc::new(RwLock::new(Vec::<BaseMessage>::new())))
        .with_registered_hooks(vec![]);
        if let Some(factory) = child_handler_factory {
            subagent = subagent.with_child_handler_factory(Arc::clone(factory));
        }
        // 能力声明：task_manager 可用时注册 AgentResultTool（collect_tools 阶段
        // 尚无 parent session，只能以布尔标记判定）
        // AssemblyContext.task_manager 为必填 Arc（上层已回退为临时实例），
        // 因此恒为可用——AgentResultTool 注册条件与迁移前（SubAgentMiddleware
        // 持 task_manager）生产路径一致。
        subagent.set_task_manager_available(true);

        // 直接构造 MiddlewareChain（顺序由 Agent 层 production_blueprint 保证）。
        // 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
        let mut chain = MiddlewareChain::new();
        for slot in blueprint {
            match slot {
                // ── 第一组：上下文注入器（system prompt 段落 / agent 定义 / 插件 / skills） ──
                ChainSlot::AgentsMd => {
                    let mut mw =
                        AgentsMdMiddleware::new().with_excludes(claude_md_excludes.clone());
                    if let Some(main) = frozen_claude_md {
                        mw = mw.with_frozen_content(main.clone(), frozen_claude_local_md.clone());
                    }
                    chain.add(Box::new(mw));
                }
                ChainSlot::AgentDefine => {
                    chain.add(Box::new(AgentDefineMiddleware::new()));
                }
                ChainSlot::Plugin => {
                    chain.add(Box::new(PluginMiddleware::new(plugin_loaded.clone())));
                }
                // 构造 SkillsMiddleware：collect_tools 提供统一 skill 协议
                // （SkillTool(skill_name) + DiscoverSkillsTool）；旧 Skill(skill, args)
                // 双协议已按 D3 移除，不再单独注册 SkillToolMiddleware。
                ChainSlot::Skills => {
                    let mut skills_mw =
                        SkillsMiddleware::new().with_plugin_roots(plugin_skill_roots.clone());
                    if let Some(summary) = frozen_skill_summary {
                        skills_mw = skills_mw.with_frozen_summary(summary.clone());
                    }
                    chain.add(Box::new(skills_mw));
                }
                ChainSlot::SkillPreload => {
                    chain.add(Box::new(
                        SkillPreloadMiddleware::new(preload_skills.clone(), cwd)
                            .with_plugin_roots(plugin_skill_roots.clone()),
                    ));
                }
                ChainSlot::AtMention => {
                    chain.add(Box::new(AtMentionMiddleware::new(cwd.clone().into())));
                }
                // 新增：图片附件处理（在 @mention 之后，将 @image <path> 转换为 ContentBlock::Image）
                ChainSlot::Image => {
                    chain.add(Box::new(ImageMiddleware::new()));
                }
                // ── 第二组：文件/终端/Web 工具提供器 ──
                ChainSlot::Filesystem => {
                    chain.add(Box::new(FilesystemMiddleware::new()));
                }
                ChainSlot::GitAttribution => {
                    chain.add(Box::new(GitAttributionMiddleware::new(model_name)));
                }
                ChainSlot::Terminal => {
                    let mut tm = TerminalMiddleware::new();
                    tm = tm.with_task_manager(Arc::clone(task_manager));
                    if let Some(ref cb) = on_bg_complete {
                        tm = tm.with_on_bg_complete(Arc::clone(cb));
                    }
                    chain.add(Box::new(tm));
                }
                ChainSlot::Web => {
                    chain.add(Box::new(WebMiddleware::new()));
                }
                // ── 第三组：Todo / Cron ──
                ChainSlot::Todo => {
                    chain.add(Box::new(TodoMiddleware::new(todo_tx.clone())));
                }
                ChainSlot::Cron => {
                    chain.add(Box::new(CronMiddleware::new(
                        cron_scheduler.clone().unwrap_or_else(|| {
                            Arc::new(parking_lot::Mutex::new(CronScheduler::new(
                                tokio::sync::mpsc::unbounded_channel().0,
                            )))
                        }),
                    )));
                }
                // ── 第四组：Hook 中间件（插件 hooks + 自定义 hooks） ──
                ChainSlot::Hook => {
                    tracing::info!(
                        groups = hook_groups.len(),
                        total_hooks = hook_groups.iter().map(|g| g.len()).sum::<usize>(),
                        session_start = session_start_source.is_some(),
                        "Builder: assembling HookMiddleware from groups"
                    );
                    if !hook_groups.is_empty() {
                        let hook_llm_factory: Arc<
                            dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync,
                        > = Arc::new({
                            let factory = llm_factory.clone();
                            move || factory(None)
                        });
                        for (i, group) in hook_groups.iter().enumerate() {
                            if group.is_empty() {
                                continue;
                            }
                            let group_size = group.len();
                            let mw = HookMiddleware::with_session_start(
                                group.clone(),
                                hook_llm_factory.clone(),
                                cwd,
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
                }
                // ── 第五组：HITL + SubAgent（条件中间件） ──
                ChainSlot::Hitl => {
                    chain.add(Box::new(HumanInTheLoopMiddleware::with_shared_mode(
                        effective_broker.clone(),
                        default_requires_approval,
                        permission_mode.clone(),
                        auto_classifier.clone(),
                    )));
                }
                // chain 与上层各持一份 SubAgentMiddleware clone：
                // 链中实例负责 collect_tools 提供 SubAgentTool；原实例由上层
                // 注入主 agent 身份（共享 cell，见 set_parent_agent_id）。
                ChainSlot::SubAgent => {
                    let subagent_for_chain = subagent.clone();
                    chain.add(Box::new(subagent_for_chain));
                }
                // ── 第六组：MCP / Workflow / ToolSearch（工具提供器） ──
                ChainSlot::Mcp => {
                    if let Some(pool) = mcp_pool {
                        chain.add(Box::new(McpMiddleware::new(Arc::clone(pool))));
                    }
                }
                // Workflow 中间件（通过 collect_tools 注册 WorkflowTool 为 deferred tool）
                ChainSlot::Workflow => {
                    if let Some(adaptor) = wf_adaptor.take() {
                        chain.add(Box::new(adaptor));
                    }
                }
                // ToolSearch 中间件
                ChainSlot::ToolSearch => {
                    chain.add(Box::new(ToolSearchMiddleware::new(
                        Arc::clone(tool_search_index),
                        Arc::clone(shared_tools),
                    )));
                }
                // ── 第七组：LSP / Goal（辅助诊断；Goal 链最后） ──
                ChainSlot::Lsp => {
                    if !lsp_servers.is_empty() {
                        let lsp_config = LspConfigFile {
                            lsp_servers: lsp_servers
                                .iter()
                                .map(|s| (s.name.clone(), s.clone()))
                                .collect(),
                        };
                        tracing::info!(
                            target: "lsp",
                            servers = lsp_config.lsp_servers.len(),
                            "LSP 中间件已注册"
                        );
                        chain.add(Box::new(LspMiddleware::new(cwd.clone(), lsp_config)));
                    }
                }
                ChainSlot::Goal => {
                    // goal active 时注入递增紧迫感 steering + 设 block_continue 让 agent 自驱续跑
                    if let Some(controller) = goal_controller {
                        let goal_mw =
                            GoalMiddleware::new(Arc::clone(controller), auxiliary_model.clone());
                        chain.add(Box::new(goal_mw));
                    }
                }
            }
        }

        // AskUserTool：v1 通过 register_tool 注册到 executor.self.tools（每轮 execute 合并）。
        // v2 stages 不调 execute()，改为一次性 insert 到 shared_tools。
        // 上层随后调 chain.collect_tools merge 时，本工具已存在不会覆盖。
        {
            let mut tools = shared_tools.write();
            tools.insert("AskUserQuestion".to_string(), Arc::new(ask_user_tool));
        }

        // 错误感知建议：从 shared_tools 构造 snapshot（所有工具都已注册）
        let all_tool_names: Vec<String> = shared_tools.read().keys().cloned().collect();
        let agents_dir = std::path::Path::new(cwd).join(".claude").join("agents");
        let agents_dir_opt = if agents_dir.exists() {
            Some(agents_dir)
        } else {
            None
        };
        let snapshot =
            error_suggest::build_tool_registry_snapshot(all_tool_names, agents_dir_opt.as_deref());
        let registry = error_suggest::build_default_registry();

        ChainAssembly {
            chain,
            subagent_mw: Some(subagent),
            error_suggest_registry: Some(registry),
            tool_registry_snapshot: Arc::new(snapshot),
        }
    }
}

// 装配触发点收敛：不再提供本层便捷入口。装配一律经 Agent 层 session 工厂的
// `build_middleware_chain`（唯一触发点，ARC-MIDDLEWARE-001）触发，
// 本模块仅保留 trait 实现（`ProductionChainAssembler`）。

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
