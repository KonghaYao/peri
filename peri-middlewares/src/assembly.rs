//! 生产中间件链装配（ARC-MIDDLEWARE-001）。
//!
//! 3.0 归位（L2）：链装配实现自 `peri-acp/src/agent/builder.rs` 迁入本模块。
//! 链序事实源位于 Agent 层 session 工厂
//! （`peri-agent/src/session/factory.rs` 的 `production_blueprint`），
//! 本模块按蓝本构造中间件实例——顺序是行为契约，禁止重排。
//!
//! 依赖方向说明（L5）：装配上下文（[`AssemblyContext`] / [`ChainAssembly`] /
//! [`OnBgCompleteFn`] / [`SystemPromptBuilder`]）随 L5 stage 装配迁入 Agent 层
//! session 工厂（事实源），middlewares 具体类型经 `peri-acp-types` 端口
//! （`McpPoolPort` / `ToolSearchPort` / `WorkflowMiddlewarePort` /
//! `CronSchedulerPort`）接入，本模块装配时 downcast 还原具体实例。

use std::{collections::HashMap, path::Path, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::{command_registry::CommandRegistry, mcp_skills::McpSkillRegistry};
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM},
    interaction::{ChannelBroker, MultiplexBroker, UserInteractionBroker},
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    session::factory::{ChainSlot, MiddlewareChainAssembler, SubAgentMiddlewarePort},
    tools::BaseTool,
};
use peri_resources::lsp::config::LspConfigFile;
use peri_resources::lsp::pool::LspServerPool;

use crate::{
    artifact::ArtifactMiddleware,
    cron::{CronMiddleware, CronScheduler, CronSchedulerPortHandle},
    default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware},
    error_suggest,
    hitl::HumanInTheLoopMiddleware,
    hooks::HookMiddleware,
    mcp::{build_tool_bridges, McpClientPool, McpMiddleware, McpResourceTool},
    middleware::{FilesystemMiddleware, TerminalMiddleware, TodoMiddleware, WebMiddleware},
    permission::{
        default_requires_approval, AutoClassifier, LlmAutoClassifier, PermissionMiddleware,
    },
    plugin::PluginMiddleware,
    ptc::PtcMiddleware,
    skills::SkillsMiddleware,
    subagent::{SkillPreloadMiddleware, SubAgentMiddleware},
    tool_search::{ToolSearchIndex, ToolSearchMiddleware},
    workflow::{WorkflowMiddleware, WorkflowMiddlewareAdaptor},
    AgentDefineMiddleware, AgentsMdMiddleware, AtMentionMiddleware, GitAttributionMiddleware,
    GitWatchMiddleware, GoalMiddleware, ImageMiddleware, LspMiddleware,
};

/// 后台任务完成回调类型（事实源 peri-agent::session::factory，L5 迁入）
pub use peri_agent::session::factory::OnBgCompleteFn;
/// System prompt 构建器类型（事实源 peri-agent::session::factory，L5 迁入）
pub use peri_agent::session::factory::SystemPromptBuilder;

/// 链装配上下文（事实源 peri-agent::session::factory，L5 迁入）。
///
/// 由 stage 装配（Agent 层 `session::exec::stage_builder`）从会话输入投影构造；
/// middlewares 具体类型经 `peri-acp-types` 端口接入，本模块装配时
/// downcast 还原（见 [`ProductionChainAssembler::assemble`]）。
pub use peri_agent::session::factory::AssemblyContext;

/// 链装配产物（事实源 peri-agent::session::factory，L5 迁入）。
pub use peri_agent::session::factory::ChainAssembly;

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
            mcp_skill_registry,
            command_registry,
            cron_scheduler,
            mcp_pool,
            dynamic_mcp,
            dynamic_mcp_projection,
            session_id,
            channel_state,
            tool_search_index,
            shared_tools,
            lsp_servers,
            lsp_pool,
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
            meta_harness_disabled,
            agent_overrides,
            language,
        } = ctx;

        // MetaHarness（设计 §2.5）：装配期关闭的 middleware 名集合。
        // 关闭判断发生在 middleware 构造之前——关闭语义要求构造副作用
        // （工具注册 / notifier 注入 / 链注册）也不存在，不能先构造再丢弃。
        let disabled: &std::collections::HashSet<String> = meta_harness_disabled;

        // L5：middlewares 具体类型经 peri-acp-types 端口接入，此处 downcast
        // 还原（端口实现方为本 crate，生产路径必成功；失败回退与原上层
        // 回退逻辑一致——临时实例 / None 降级）。

        // Cron 调度器：端口 → Arc<Mutex<CronScheduler>>（CronMiddleware 消费）。
        // downcast 失败或无注入时构造临时实例（行为与迁移前一致）。
        let cron_scheduler_concrete: Option<Arc<parking_lot::Mutex<CronScheduler>>> =
            cron_scheduler.as_ref().map(|p| {
                Arc::clone(p)
                    .downcast_arc::<CronSchedulerPortHandle>()
                    .map(|h| h.0.clone())
                    .unwrap_or_else(|_| {
                        Arc::new(parking_lot::Mutex::new(CronScheduler::new(
                            tokio::sync::mpsc::unbounded_channel().0,
                        )))
                    })
            });

        // MCP 连接池：端口 → Arc<McpClientPool>。downcast 失败按未注入处理
        //（不注册 MCP 中间件/工具）。
        let mcp_pool_concrete: Option<Arc<McpClientPool>> = mcp_pool.as_ref().map(|p| {
            Arc::clone(p)
                .downcast_arc::<McpClientPool>()
                .unwrap_or_else(|_| Arc::new(McpClientPool::new_pending()))
        });
        let mcp_agent_registry = mcp_pool_concrete
            .as_ref()
            .map(|pool| Arc::new(crate::mcp::McpAgentRegistry::new(Arc::clone(pool))));

        // 工具搜索索引：端口 → Arc<ToolSearchIndex>（失败回退默认实例）。
        let tool_search_index_concrete: Arc<ToolSearchIndex> = Arc::clone(tool_search_index)
            .downcast_arc::<ToolSearchIndex>()
            .unwrap_or_else(|_| Arc::new(ToolSearchIndex::default()));

        // WorkflowMiddleware 端口（会话级复用，None 时构造临时实例）。
        let workflow_middleware_concrete: Option<Arc<WorkflowMiddleware>> = workflow_middleware
            .as_ref()
            .and_then(|p| Arc::clone(p).downcast_arc::<WorkflowMiddleware>().ok());

        // HITL middleware — reuse auto_classifier model from cache when available
        let auto_classifier: Option<Arc<dyn AutoClassifier>> = Some(Arc::new(
            LlmAutoClassifier::new(auto_classifier_model.clone()),
        ));
        // 构造 permission broker（当 channel_state 存在时用 MultiplexBroker 包装）
        let effective_broker: Arc<dyn UserInteractionBroker> =
            match (channel_state, mcp_pool_concrete.as_ref()) {
                (Some(cs), Some(pool)) => {
                    let pool_arc: Arc<McpClientPool> = Arc::clone(pool);
                    let sender: Arc<dyn peri_agent::interaction::ChannelNotificationSender> =
                        pool_arc;
                    let channel_broker = Arc::new(ChannelBroker::new(cs.clone(), sender));
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

        // AskUser 工具（2026-08-15 拆分后）由链上 HumanInTheLoopMiddleware
        // 的 collect_tools 提供（使用原始 broker 而非 MultiplexBroker——
        // ChannelBroker 对 Questions 立即返回空答案、MultiplexBroker 竞速时
        // Channel 总是先返回，导致 AskUserQuestion 弹窗被绕过）；宿主级
        // shared_tools 不再注册任何工具。

        // 父工具集（供子 agent 继承）。MetaHarness：父工具按持有 middleware
        // 分支构造——关闭的 middleware 连坐，其工具不进入 parent_tools
        // （设计 §2.5"关闭面 = 全部装配入口"）。
        let mut parent_tools: Vec<Box<dyn BaseTool>> = Vec::new();
        if !disabled.contains("FilesystemMiddleware") {
            parent_tools.extend(FilesystemMiddleware::build_tools(cwd));
        }
        if !disabled.contains("TerminalMiddleware") {
            parent_tools.extend(TerminalMiddleware::build_tools(cwd));
        }
        if !disabled.contains("WebMiddleware") {
            parent_tools.extend(WebMiddleware::build_tools());
        }
        if !disabled.contains("McpMiddleware") {
            if let Some(ref pool) = mcp_pool_concrete {
                let mcp_tools = build_tool_bridges(pool);
                for tool in mcp_tools {
                    parent_tools.push(tool);
                }
                if pool.has_resources() {
                    parent_tools.push(Box::new(McpResourceTool::new(
                        Arc::clone(pool),
                        // 未装配 session 注册表（print 模式）→ 空注册表
                        //（无条目 = 不校验）
                        mcp_skill_registry
                            .clone()
                            .unwrap_or_else(|| Arc::new(McpSkillRegistry::new())),
                    )));
                }
            }
        }

        // Workflow 中间件（条件注册）
        // 优先复用 session 级 WorkflowMiddleware（progress_store/registry/runner 跨 turn 存活）。
        // 仅在无 session 级实例时创建临时实例（print 模式等）。
        // MetaHarness：WorkflowMiddleware 关闭 → 不构造临时/复用 adaptor
        // （设计 §2.5，构造副作用与链注册同时消失）。
        let mut wf_adaptor: Option<WorkflowMiddlewareAdaptor> = None;
        if !disabled.contains("WorkflowMiddleware") {
            if let Some(ref executor) = workflow_executor {
                let wf_mw = if let Some(ref session_mw) = workflow_middleware_concrete {
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
        }

        // SubAgent middleware（L3 瘦身：只声明工具与发起意图）。
        // [TRAP] SubAgent 复用 main agent 在 session/new 时捕获的 frozen CLAUDE.md/Skills
        // （L3 起由 Agent 层 spawn_subagent 从父 session copy，此处不再透传）；
        // 运行时通道（thread_store / task_manager / bg_event_sender / register /
        // deregister / langfuse_bridge / frozen 回退）统一经 SubagentHost 注入
        // 主 session（builder 侧构造），此处只留工具声明字段。
        // MetaHarness：SubAgentMiddleware 关闭 → 关联构造联动置空
        // （parent_tools 不注入、subagent_mw 槽位 None、链上不注册——禁止半开
        // 状态，设计 §2.5"联动清理"）。
        let mut subagent: Option<SubAgentMiddleware> = if disabled.contains("SubAgentMiddleware") {
            None
        } else {
            Some(
                SubAgentMiddleware::new(
                    parent_tools,
                    Some(Arc::clone(event_handler) as Arc<dyn AgentEventHandler>),
                    llm_factory.clone(),
                )
                .with_plugin_agent_dirs(
                    plugin_loaded
                        .iter()
                        .flat_map(|plugin| plugin.agents_dirs.clone())
                        .collect(),
                )
                .with_mcp_agents(mcp_agent_registry.clone(), Arc::clone(broker))
                .with_system_builder(system_builder.clone())
                .with_cancel(cancel.clone())
                .with_parent_messages(Arc::new(RwLock::new(Vec::<BaseMessage>::new())))
                .with_registered_hooks(vec![]),
            )
        };
        if let Some(ref mut mw) = subagent {
            if let Some(factory) = child_handler_factory {
                *mw = mw.clone().with_child_handler_factory(Arc::clone(factory));
            }
            // 能力声明：task_manager 可用时注册 AgentResultTool（collect_tools 阶段
            // 尚无 parent session，只能以布尔标记判定）
            // AssemblyContext.task_manager 为必填 Arc（上层已回退为临时实例），
            // 因此恒为可用——AgentResultTool 注册条件与迁移前（SubAgentMiddleware
            // 持 task_manager）生产路径一致。
            mw.set_task_manager_available(true);
        }

        // 直接构造 MiddlewareChain（顺序由 Agent 层 production_blueprint 保证）。
        // 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
        let mut chain = MiddlewareChain::new();
        for slot in blueprint {
            match slot {
                // ── MetaHarness（设计 §2.5）：关闭的 middleware 不构造、不进链。
                // 判断先于构造——关闭语义要求构造副作用也不存在。
                // ── 波 4 演进 2：基础系统提示词段持有者（内容载体；渲染走
                // PromptTemplate 段落装配，链序不参与渲染排序——契约 2）──
                ChainSlot::DefaultSystemPrompt
                    if disabled.contains("DefaultSystemPromptMiddleware") => {}
                ChainSlot::DefaultSystemPrompt => {
                    chain.add(Box::new(DefaultSystemPromptMiddleware::new(
                        agent_overrides.clone(),
                    )));
                }
                ChainSlot::Lang if disabled.contains("LangMiddleware") => {}
                ChainSlot::Lang => {
                    chain.add(Box::new(LangMiddleware::new(language.clone())));
                }
                // ── 第一组：上下文注入器（system prompt 段落 / agent 定义 / 插件 / skills） ──
                ChainSlot::AgentsMd if disabled.contains("AgentsMdMiddleware") => {}
                ChainSlot::AgentsMd => {
                    let mut mw =
                        AgentsMdMiddleware::new().with_excludes(claude_md_excludes.clone());
                    if let Some(main) = frozen_claude_md {
                        mw = mw.with_frozen_content(main.clone(), frozen_claude_local_md.clone());
                    }
                    chain.add(Box::new(mw));
                }
                ChainSlot::AgentDefine if disabled.contains("AgentDefineMiddleware") => {}
                ChainSlot::AgentDefine => {
                    chain.add(Box::new(AgentDefineMiddleware::new()));
                }
                ChainSlot::Plugin if disabled.contains("PluginMiddleware") => {}
                ChainSlot::Plugin => {
                    chain.add(Box::new(PluginMiddleware::new(plugin_loaded.clone())));
                }
                // 构造 SkillsMiddleware：collect_tools 提供统一 skill 协议
                // （SkillTool(skill_name) + DiscoverSkillsTool）；旧 Skill(skill, args)
                // 双协议已按 D3 移除，不再单独注册 SkillToolMiddleware。
                ChainSlot::Skills if disabled.contains("SkillsMiddleware") => {}
                ChainSlot::Skills => {
                    let mut skills_mw = SkillsMiddleware::new()
                        .with_plugin_roots(plugin_skill_roots.clone())
                        .with_mcp_registry(ctx.mcp_skill_registry.clone());
                    if let Some(summary) = frozen_skill_summary {
                        skills_mw = skills_mw.with_frozen_summary(summary.clone());
                    }
                    chain.add(Box::new(skills_mw));
                }
                ChainSlot::SkillPreload if disabled.contains("SkillPreloadMiddleware") => {}
                ChainSlot::SkillPreload => {
                    chain.add(Box::new(
                        SkillPreloadMiddleware::new(preload_skills.clone(), cwd)
                            .with_plugin_roots(plugin_skill_roots.clone())
                            .with_mcp_registry(ctx.mcp_skill_registry.clone()),
                    ));
                }
                ChainSlot::AtMention if disabled.contains("AtMentionMiddleware") => {}
                ChainSlot::AtMention => {
                    chain.add(Box::new(AtMentionMiddleware::new(cwd.clone().into())));
                }
                // 新增：图片附件处理（在 @mention 之后，将 @image <path> 转换为 ContentBlock::Image）
                ChainSlot::Image if disabled.contains("ImageMiddleware") => {}
                ChainSlot::Image => {
                    chain.add(Box::new(ImageMiddleware::new()));
                }
                // ── 第二组：文件/终端/Web 工具提供器 ──
                ChainSlot::Filesystem if disabled.contains("FilesystemMiddleware") => {}
                ChainSlot::Filesystem => {
                    chain.add(Box::new(FilesystemMiddleware::new()));
                }
                ChainSlot::GitAttribution if disabled.contains("GitAttributionMiddleware") => {}
                ChainSlot::GitAttribution => {
                    chain.add(Box::new(GitAttributionMiddleware::new(model_name)));
                }
                ChainSlot::GitWatch if disabled.contains("GitWatchMiddleware") => {}
                ChainSlot::GitWatch => {
                    chain.add(Box::new(GitWatchMiddleware::new()));
                }
                ChainSlot::Terminal if disabled.contains("TerminalMiddleware") => {}
                ChainSlot::Terminal => {
                    let mut tm = TerminalMiddleware::new();
                    tm = tm.with_task_manager(
                        Arc::clone(task_manager) as Arc<dyn peri_acp_types::tasks::TaskManager>
                    );
                    if let Some(ref cb) = on_bg_complete {
                        tm = tm.with_on_bg_complete(Arc::clone(cb));
                    }
                    chain.add(Box::new(tm));
                }
                ChainSlot::Web if disabled.contains("WebMiddleware") => {}
                ChainSlot::Web => {
                    chain.add(Box::new(WebMiddleware::new()));
                }
                // ── 第三组：Todo / Cron ──
                ChainSlot::Todo if disabled.contains("TodoMiddleware") => {}
                ChainSlot::Todo => {
                    chain.add(Box::new(TodoMiddleware::new(todo_tx.clone())));
                }
                ChainSlot::Cron if disabled.contains("CronMiddleware") => {}
                ChainSlot::Cron => {
                    chain.add(Box::new(CronMiddleware::new(
                        cron_scheduler_concrete.clone().unwrap_or_else(|| {
                            Arc::new(parking_lot::Mutex::new(CronScheduler::new(
                                tokio::sync::mpsc::unbounded_channel().0,
                            )))
                        }),
                    )));
                }
                // ── 第四组：Hook 中间件（插件 hooks + 自定义 hooks） ──
                // MetaHarness：Hook 关闭 → 全部 hook group 都不构造。
                ChainSlot::Hook if disabled.contains("HookMiddleware") => {}
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
                // ── 第五组：Permission + AskUser(HITL) + SubAgent（条件中间件） ──
                // 2026-08-15 职责拆分（spec/issues/2026-08-15-permission-hitl-split.md）：
                // PermissionMiddleware = 审批钩子（10_hitl 段落）；新
                // HumanInTheLoopMiddleware = 提问通道（AskUserQuestion 工具 +
                // 12_ask_user 段落），各自独立关闭——关闭提问 → AskUserQuestion
                // 不进链 → 每 turn 本地视图不含（"关闭不掉"修复）。
                ChainSlot::Permission if disabled.contains("PermissionMiddleware") => {}
                ChainSlot::Permission => {
                    chain.add(Box::new(PermissionMiddleware::with_shared_mode(
                        effective_broker.clone(),
                        default_requires_approval,
                        permission_mode.clone(),
                        auto_classifier.clone(),
                    )));
                }
                ChainSlot::AskUser if disabled.contains("HumanInTheLoopMiddleware") => {}
                ChainSlot::AskUser => {
                    // 使用原始 broker（非 MultiplexBroker）：ChannelBroker 对
                    // Questions 立即返回空答案、Multiplex 竞速时 Channel 先
                    // 返回，会绕过 TUI 弹窗（既有约束，见 189-192 注释）。
                    chain.add(Box::new(HumanInTheLoopMiddleware::new(broker.clone())));
                }
                // chain 与上层各持一份 SubAgentMiddleware clone：
                // 链中实例负责 collect_tools 提供 SubAgentTool；原实例由上层
                // 注入主 agent 身份（共享 cell，见 set_parent_agent_id）。
                // MetaHarness：SubAgentMiddleware 关闭 → 链上不注册（subagent_mw
                // 槽位在下方联动置 None）。
                ChainSlot::SubAgent if disabled.contains("SubAgentMiddleware") => {}
                ChainSlot::SubAgent => {
                    if let Some(mw) = subagent.as_ref() {
                        let subagent_for_chain = mw.clone();
                        chain.add(Box::new(subagent_for_chain));
                    }
                }
                // ── 第六组：MCP / Workflow / ToolSearch（工具提供器） ──
                // MetaHarness：McpMiddleware 关闭 → 即使 pool 存在也不构造、
                // 不设置 notifier（构造副作用消失）。
                ChainSlot::Mcp if disabled.contains("McpMiddleware") => {}
                ChainSlot::Mcp => {
                    if let Some(deployment) = dynamic_mcp.as_ref() {
                        chain.add(Box::new(crate::mcp::dynamic::DynamicMcpMiddleware::new(
                            session_id.clone(),
                            Arc::clone(deployment),
                        )));
                    }
                    if let Some(pool) = mcp_pool_concrete.as_ref() {
                        let effective_pool = if let Some(deployment) = dynamic_mcp.as_ref() {
                            let mut projection_holder = dynamic_mcp_projection.lock();
                            if let Some(existing) = projection_holder.as_ref() {
                                existing
                                    .as_any()
                                    .downcast_ref::<crate::mcp::dynamic::registry::CheckedSessionMcpProjection>()
                                    .map(|projection| projection.pool())
                                    .unwrap_or_else(|| Arc::clone(pool))
                            } else {
                                let static_handles = pool
                                    .get_all_clients()
                                    .into_iter()
                                    .map(|handle| {
                                        let token: peri_acp_types::mcp_skills::HandleToken =
                                            handle.clone();
                                        (handle.name.clone(), token)
                                    })
                                    .collect();
                                let skill_registry = ctx
                                    .mcp_skill_registry
                                    .clone()
                                    .unwrap_or_else(|| Arc::new(McpSkillRegistry::new()));
                                let command_registry = command_registry
                                    .clone()
                                    .unwrap_or_else(|| Arc::new(CommandRegistry::new()));
                                let lease = deployment.capability(session_id).bind_projection(
                                    static_handles,
                                    skill_registry,
                                    command_registry,
                                );
                                let projected = lease
                                    .as_any()
                                    .downcast_ref::<crate::mcp::dynamic::registry::CheckedSessionMcpProjection>()
                                    .map(|projection| projection.pool())
                                    .unwrap_or_else(|| Arc::clone(pool));
                                *projection_holder = Some(lease);
                                projected
                            }
                        } else {
                            Arc::clone(pool)
                        };
                        let mw = McpMiddleware::new(Arc::clone(&effective_pool))
                            .with_tool_pool(Arc::clone(pool))
                            .with_skill_discovery(
                                ctx.mcp_skill_registry.clone(),
                                ctx.cancel.clone(),
                            )
                            .with_command_registry(command_registry.clone());
                        // 决策 B：装配后立即触发幂等发现（覆盖「装配时连接已
                        // 完成」的场景——已连接 server 即刻 spawn 发现，命令
                        // 面/元数据面无需等首轮 before_agent；Started 去重 /
                        // Completed 跳过 / Arc::ptr_eq 重连检测保证幂等）。
                        mw.ensure_discovery();
                        // 注入状态变化通知：经 session 事件通道发布
                        // system-notification（TUI 通知面显示）。pool 全局共享，
                        // 多 session 时以最后装配的 session 通道为准。
                        // 连接完成事件（决策 B）：Connected 状态变化经
                        // record_status_change → notifier 补偿触发发现——
                        // 覆盖重连/OAuth 授权后连接的场景。注意两个边界：
                        // (1) 补偿以最后装配 session 的 cancel 生命周期为限
                        // （cancel 后入口早退，发现延迟到其他 session 的
                        // before_agent）；(2) 初始连接事件不经过
                        // record_status_change（run_initialize 直接插入
                        // Connected handle），由 run_initialize 收口时
                        // notify_initial_connections 补发一次——「刚进入、
                        // 未说话」时初始连接的 server 也能立即驱动发现，
                        // 四挂点（装配后立即/session 预热/连接事件/
                        // before_agent）+ 初始化收口补发覆盖全时序。
                        let tx = ctx.bg_event_tx.clone();
                        crate::mcp::middleware::attach_connection_notifier(
                            &effective_pool,
                            ctx.mcp_skill_registry.as_ref(),
                            command_registry.as_ref(),
                            &ctx.cancel,
                            Some(tx),
                        );
                        chain.add(Box::new(mw));
                    }
                }
                // Workflow 中间件（通过 collect_tools 注册 WorkflowTool 为 deferred tool）
                // MetaHarness：WorkflowMiddleware 关闭 → wf_adaptor 已为 None，不注册。
                ChainSlot::Workflow if disabled.contains("WorkflowMiddleware") => {}
                ChainSlot::Workflow => {
                    if let Some(adaptor) = wf_adaptor.take() {
                        chain.add(Box::new(adaptor));
                    }
                }
                // Programmatic Tool Calling：注册 deferred RunPtcCode，由 ToolSearch 发现/执行。
                ChainSlot::Ptc if disabled.contains("PtcMiddleware") => {}
                ChainSlot::Ptc => {
                    chain.add(Box::new(PtcMiddleware::new()));
                }
                // ToolSearch 中间件
                ChainSlot::ToolSearch if disabled.contains("ToolSearch") => {}
                ChainSlot::ToolSearch => {
                    chain.add(Box::new(ToolSearchMiddleware::new(
                        Arc::clone(&tool_search_index_concrete),
                        Arc::clone(shared_tools),
                    )));
                }
                // Artifact 中间件：独立关闭不影响 ToolSearch 元工具。
                ChainSlot::Artifact if disabled.contains("ArtifactMiddleware") => {}
                ChainSlot::Artifact => {
                    chain.add(Box::new(ArtifactMiddleware::new()));
                }
                // ── 第七组：LSP / Goal（辅助诊断；Goal 链最后） ──
                // MetaHarness：Lsp / Goal 关闭 → 即使运行条件满足也不构造。
                ChainSlot::Lsp if disabled.contains("LspMiddleware") => {}
                ChainSlot::Lsp => {
                    if !lsp_servers.is_empty() {
                        // 会话级 pool 复用（workflow_middleware 同构模式，H1）：
                        // Some → 复用跨 turn 存活的 pool（服务器进程/initialized/
                        // 诊断状态不丢）；None → 临时实例（print 模式等无 session 路径）。
                        let lsp_mw = if let Some(pool) = lsp_pool
                            .as_ref()
                            .and_then(|p| Arc::clone(p).downcast_arc::<LspServerPool>().ok())
                        {
                            LspMiddleware::from_pool(pool)
                        } else {
                            let lsp_config = LspConfigFile {
                                lsp_servers: lsp_servers
                                    .iter()
                                    .map(|s| (s.name.clone(), s.clone()))
                                    .collect(),
                            };
                            tracing::info!(
                                target: "lsp",
                                servers = lsp_config.lsp_servers.len(),
                                "LSP 中间件已注册（临时 pool）"
                            );
                            LspMiddleware::new(cwd.clone(), lsp_config)
                        };
                        chain.add(Box::new(lsp_mw));
                    }
                }
                ChainSlot::Goal if disabled.contains("GoalMiddleware") => {}
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
            // MetaHarness：SubAgentMiddleware 关闭 → 槽位联动置空（禁止半开状态）。
            subagent_mw: subagent.map(|mw| Arc::new(mw) as Arc<dyn SubAgentMiddlewarePort>),
            error_suggest_registry: Some(registry),
            tool_registry_snapshot: Arc::new(snapshot),
        }
    }
}

// 装配触发点收敛：不再提供本层便捷入口。装配一律经 Agent 层 session 工厂的
// `build_middleware_chain`（唯一触发点，ARC-MIDDLEWARE-001）触发，
// 本模块仅保留 trait 实现（`ProductionChainAssembler`）。

// ── Workflow agent 装配端口实现（p1-wa 收口）──────────────────────────────
//
// 实现 `peri_agent::agent::workflow::WorkflowMiddlewareFactory`（§0
// Middleware → Agent 声明边）：workflow agent 执行体所需的中间件链 / 工具
// 列表 / error_suggest / tool resolver / session 级 WorkflowMiddleware 实例
// 装配全部收拢在本节（自 `peri-acp::host::workflow_agent` 执行本体迁出）；
// ACP 宿主装配点（`host/assemble.rs` 经 TUI 部署装配点 / `host/stdio`
// 装配面）构造本工厂后 upcast 为端口注入。
//
// 链序/工具集与迁移前 `create_session_workflow_middleware` /
// `WorkflowAgentExecutor::execute` 内装配完全一致（行为契约，禁止重排）。

use peri_acp_types::ports::{LspPoolPort, WorkflowMiddlewarePort};
use peri_acp_types::workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult};
use peri_agent::agent::workflow::{
    WorkflowAgentContext, WorkflowAgentDefinition, WorkflowMiddlewareFactory,
};
use peri_agent::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::tools::ToolInvocationResolver;

/// 加载全局 LSP 配置（settings.json 的 `config.lspServers`）并与插件 LSP
/// 服务器合并，返回装配用服务器列表。
///
/// 合并优先级对齐 MCP 三层合并（`crate::mcp::config::load_merged_config_full`）：
/// global < plugin——同名 key 插件覆盖全局（插件名带 `plugin:{name}:{server}`
/// 前缀，实际冲突面小，覆盖方向仍与 MCP 一致）。source 标记与 `${VAR}`
/// 展开由加载/构造侧完成（`load_global_lsp_config` / `lsp_config_from_plugin`），
/// 此处只做合并。无任何配置时返回空 Vec——装配处
/// `lsp_servers.is_empty()` 条件注册语义不变。
///
/// H5：宿主装配（TUI/print 经 `assemble_server_config`、stdio 经
/// `init_stdio_context`）经此函数接入全局配置；此前宿主只取插件
/// lsp_servers，无插件时 LSP 整条产品线静默不可用。
pub fn load_merged_lsp_servers(
    settings_json_path: &Path,
    plugin_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
) -> Vec<peri_acp_types::lsp::LspServerConfig> {
    let global = peri_resources::lsp::config::load_global_lsp_config(settings_json_path);
    let mut merged: HashMap<String, peri_acp_types::lsp::LspServerConfig> = global.lsp_servers;
    for server in plugin_servers {
        merged.insert(server.name.clone(), server);
    }
    merged.into_values().collect()
}

/// 构造会话级 LSP 服务器池并 upcast 端口（装配面宿主 session/new /
/// load / resume / fork 调用；返回类型已锚定端口 trait，调用方无需引用
/// peri-lsp 类型路径）。
///
/// 无服务器配置时返回 None（不注册 LSP 中间件，与装配面
/// `lsp_servers.is_empty()` 条件注册语义一致）。H1：会话级实例跨 turn
/// 复用（服务器进程 / initialized / 诊断状态不丢），宿主退出时经端口
/// `shutdown` 优雅关闭。
pub fn create_session_lsp_pool(
    cwd: &str,
    configs: &[peri_acp_types::lsp::LspServerConfig],
) -> Option<Arc<dyn LspPoolPort>> {
    if configs.is_empty() {
        return None;
    }
    let lsp_config = LspConfigFile {
        lsp_servers: configs
            .iter()
            .map(|s| (s.name.clone(), s.clone()))
            .collect(),
    };
    Some(Arc::new(LspServerPool::new(cwd, lsp_config)) as Arc<dyn LspPoolPort>)
}

/// workflow agent 装配工厂（ZST：无状态装配器）。
pub struct WorkflowAgentMiddlewareFactory;

/// 构造 workflow agent 装配端口并 upcast（部署装配点调用；返回类型已锚定
/// 端口 trait，调用方无需引用 peri-agent 类型路径——TUI 等消费方只写
/// `peri_middlewares::assembly::default_workflow_middleware_factory()`）。
pub fn default_workflow_middleware_factory(
) -> Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory> {
    Arc::new(WorkflowAgentMiddlewareFactory)
}

impl WorkflowMiddlewareFactory for WorkflowAgentMiddlewareFactory {
    fn resolve_agent_definition(
        &self,
        agent_type: &str,
        cwd: &str,
    ) -> Result<WorkflowAgentDefinition, String> {
        let project_path = AgentDefineMiddleware::candidate_paths(cwd, agent_type)
            .into_iter()
            .find(|path| path.is_file());
        let agent = if let Some(path) = project_path {
            let content = std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "failed to read agent definition '{}': {error}",
                    path.display()
                )
            })?;
            crate::parse_agent_file(&content)
                .ok_or_else(|| format!("failed to parse agent definition '{}'", path.display()))?
        } else {
            let built_in = crate::subagent::get_built_in_agent(agent_type)
                .ok_or_else(|| format!("cannot find agent definition '{agent_type}'"))?;
            crate::parse_agent_file(built_in.content).ok_or_else(|| {
                format!("failed to parse built-in agent definition '{agent_type}'")
            })?
        };
        let frontmatter = agent.frontmatter;
        let prompt_overrides = {
            let overrides = crate::AgentOverrides {
                persona: (!agent.system_prompt.is_empty()).then_some(agent.system_prompt),
                tone: frontmatter.tone.clone(),
                proactiveness: frontmatter.proactiveness.clone(),
                mode: frontmatter.prompt_mode.clone(),
            };
            (!overrides.is_empty()).then_some(overrides)
        };
        let model = frontmatter
            .model
            .filter(|model| !model.is_empty() && model != "inherit");
        let allowed_tools = match frontmatter.tools {
            crate::ToolsValue::Empty => None,
            tools => Some(tools.to_vec()),
        };
        Ok(WorkflowAgentDefinition {
            model,
            allowed_tools,
            disallowed_tools: frontmatter.disallowed_tools.to_vec(),
            skill_names: frontmatter.skills,
            allowed_write_dirs: frontmatter.allowed_write_dirs,
            max_iterations: frontmatter.max_turns.unwrap_or(200) as usize,
            prompt_overrides,
        })
    }

    fn build_tools(
        &self,
        cwd: &str,
        disabled: &std::collections::HashSet<String>,
    ) -> Vec<Box<dyn BaseTool>> {
        let mut tools: Vec<Box<dyn BaseTool>> = Vec::new();
        // MetaHarness（设计 §2.5）：关闭的 middleware 连坐，其工具不进列表。
        if !disabled.contains("FilesystemMiddleware") {
            tools.extend(FilesystemMiddleware::build_tools(cwd));
        }
        if !disabled.contains("TerminalMiddleware") {
            tools.extend(TerminalMiddleware::build_tools(cwd));
        }
        if !disabled.contains("WebMiddleware") {
            tools.extend(WebMiddleware::build_tools());
        }
        // Workflow agent 无 plugin_skill_roots，仅 project-level skill 可用。
        // 在注册工具前扫描 project skills，预填充缓存（SkillTool 无懒扫描回退）。
        // D3：统一模型可见协议为 SkillTool(skill_name) + DiscoverSkillsTool，
        // 与主 agent / subagent 链一致，不再注册旧 Skill(skill, args)。
        if !disabled.contains("SkillsMiddleware") {
            let project_skills_root = std::path::PathBuf::from(cwd).join(".claude").join("skills");
            let skills = crate::skills::loader::scan_skill_roots(&[crate::skills::SkillRoot {
                path: project_skills_root,
                source: crate::skills::SkillSource::Project,
                plugin_name: None,
            }]);
            let cached = std::sync::Arc::new(std::sync::RwLock::new(if skills.is_empty() {
                None
            } else {
                Some(skills)
            }));
            tools.push(Box::new(crate::skills::tools::SkillTool::new(Arc::clone(
                &cached,
            ))));
            tools.push(Box::new(crate::skills::tools::DiscoverSkillsTool::new(
                cached,
            )));
        }
        tools
    }

    fn build_sandbox_write_tool(
        &self,
        cwd: &str,
        allowed_dirs: &[String],
    ) -> Option<Box<dyn BaseTool>> {
        match crate::tools::filesystem::WriteSandboxTool::new(cwd, allowed_dirs.to_vec()) {
            Ok(tool) => Some(Box::new(tool)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    sandbox_dirs = ?allowed_dirs,
                    "workflow agent: failed to construct SandboxWrite"
                );
                None
            }
        }
    }

    fn build_middlewares(
        &self,
        ctx: &WorkflowAgentContext,
        model_name: &str,
        skill_names: &[String],
    ) -> Vec<Box<dyn Middleware>> {
        let mut middlewares: Vec<Box<dyn Middleware>> = Vec::new();

        // MetaHarness（设计 §2.5）：workflow agent 链独立装配，关闭面同样生效；
        // 未禁用项保持原相对顺序（行为契约，禁止重排）。
        let disabled = &ctx.meta_harness_disabled;

        if !disabled.contains("AgentsMdMiddleware") {
            let mut agents_md = AgentsMdMiddleware::new();
            if let Some(ref md) = ctx.frozen_claude_md {
                agents_md =
                    agents_md.with_frozen_content(md.clone(), ctx.frozen_claude_local_md.clone());
            }
            middlewares.push(Box::new(agents_md));
        }

        if !disabled.contains("SkillsMiddleware") {
            let mut skills_mw = SkillsMiddleware::new();
            if let Some(ref summary) = ctx.frozen_skill_summary {
                skills_mw = skills_mw.with_frozen_summary(summary.clone());
            }
            middlewares.push(Box::new(skills_mw));
        }

        // 与普通 subagent 一致：agent.md 声明的 skills 在启动时预加载。
        if !disabled.contains("SkillPreloadMiddleware") {
            middlewares.push(Box::new(SkillPreloadMiddleware::new(
                skill_names.to_vec(),
                &ctx.cwd,
            )));
        }

        if !disabled.contains("FilesystemMiddleware") {
            middlewares.push(Box::new(FilesystemMiddleware::new()));
        }

        // 3a. GitAttributionMiddleware（在 FilesystemMiddleware 之后）
        if !disabled.contains("GitAttributionMiddleware") {
            middlewares.push(Box::new(GitAttributionMiddleware::new(model_name)));
        }

        if !disabled.contains("TerminalMiddleware") {
            middlewares.push(Box::new(TerminalMiddleware::new()));
        }
        if !disabled.contains("WebMiddleware") {
            middlewares.push(Box::new(WebMiddleware::new()));
        }

        // 3b. TodoMiddleware（在 WebMiddleware 之后）
        if !disabled.contains("TodoMiddleware") {
            let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<crate::tools::TodoItem>>(8);
            middlewares.push(Box::new(TodoMiddleware::new(todo_tx)));
        }

        // GAP-03: PermissionMiddleware（审批，原 HITL 审批职责）。
        // broker + permission_mode 均 Some 时启用审批（遵循 session 权限模式）；
        // 否则 Bypass（自主后台 agent 默认行为）。
        if !disabled.contains("PermissionMiddleware") {
            let permission = match (&ctx.broker, &ctx.permission_mode) {
                (Some(broker), Some(mode)) => PermissionMiddleware::with_shared_mode(
                    Arc::clone(broker),
                    default_requires_approval,
                    Arc::clone(mode),
                    None, // auto_classifier: workflow agent 不需要 LLM 分类器
                ),
                _ => PermissionMiddleware::disabled(),
            };
            middlewares.push(Box::new(permission));
        }
        // 提问通道（新 HumanInTheLoopMiddleware，含 AskUserQuestion）：
        // workflow agent 的 broker 恒 None（advisor 裁决 B：workflow 链不
        // 装配 HITL，`workflow_agent.rs` / `agent.rs` 构造点），此处不装配
        // ——AskUserQuestion 随 2026-08-15 拆分从 workflow agent 消失
        // （旧行为经宿主级 shared_tools 泄漏 TUI broker 到后台 agent，
        // 非有意设计，见 spec/issues/2026-08-15-permission-hitl-split.md）。
        if !disabled.contains("HumanInTheLoopMiddleware") {
            if let Some(broker) = &ctx.broker {
                middlewares.push(Box::new(HumanInTheLoopMiddleware::new(Arc::clone(broker))));
            }
        }

        // [v2] CompactMiddleware 已移除——Workflow agent 的自动 compact 由 v2
        // stages/compact.rs 统一接管（run_react_loop 在每轮开头调 compact_v2::run_compact）。

        middlewares
    }

    fn build_tool_resolver(&self) -> Arc<dyn ToolInvocationResolver> {
        Arc::new(crate::tool_search::ExecuteExtraToolResolver::default())
    }

    fn build_error_suggest(
        &self,
        cwd: &str,
        tool_names: &[String],
    ) -> (Arc<ErrorSuggestRegistry>, ToolRegistrySnapshot) {
        let agents_dir = std::path::Path::new(cwd).join(".claude").join("agents");
        let agents_dir_opt = if agents_dir.exists() {
            Some(agents_dir.as_path())
        } else {
            None
        };
        let snapshot = crate::error_suggest::build_tool_registry_snapshot(
            tool_names.iter().cloned(),
            agents_dir_opt,
        );
        (crate::error_suggest::build_default_registry(), snapshot)
    }

    fn build_workflow_middleware(
        &self,
        executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
        progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
    ) -> Arc<dyn WorkflowMiddlewarePort> {
        Arc::new(WorkflowMiddleware::new(
            executor,
            cwd,
            notification_tx,
            progress_rx,
        ))
    }
}

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
