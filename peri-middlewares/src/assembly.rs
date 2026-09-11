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

mod hooks;
mod lsp;
mod mcp;
mod preparation;
mod prompt;
mod workflow;

pub use lsp::{create_session_lsp_pool, load_merged_lsp_servers};
pub use workflow::{default_workflow_middleware_factory, WorkflowAgentMiddlewareFactory};

use crate::{
    artifact::ArtifactMiddleware,
    cron::{CronMiddleware, CronScheduler},
    default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware},
    error_suggest,
    hitl::HumanInTheLoopMiddleware,
    middleware::{FilesystemMiddleware, TerminalMiddleware, TodoMiddleware, WebMiddleware},
    permission::{default_requires_approval, PermissionMiddleware},
    plugin::PluginMiddleware,
    ptc::PtcMiddleware,
    subagent::SubAgentMiddleware,
    tool_search::ToolSearchMiddleware,
    workflow::{WorkflowMiddleware, WorkflowMiddlewareAdaptor},
    AgentDefineMiddleware, AtMentionMiddleware, GitAttributionMiddleware, GitWatchMiddleware,
    GoalMiddleware, ImageMiddleware,
};
use parking_lot::RwLock;
use peri_agent::{
    agent::events::AgentEventHandler,
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    session::factory::{ChainSlot, MiddlewareChainAssembler, SubAgentMiddlewarePort},
};
use std::sync::Arc;

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
            auxiliary_model,
            plugin_loaded,
            workflow_executor,
            event_handler,
            task_manager,
            on_bg_complete,
            child_handler_factory,
            llm_factory,
            system_builder,
            todo_tx,
            goal_controller,
            meta_harness_disabled,
            agent_overrides,
            language,
            shared_tools,
            ..
        } = ctx;

        // MetaHarness（设计 §2.5）：装配期关闭的 middleware 名集合。
        // 关闭判断发生在 middleware 构造之前——关闭语义要求构造副作用
        // （工具注册 / notifier 注入 / 链注册）也不存在，不能先构造再丢弃。
        let disabled: &std::collections::HashSet<String> = meta_harness_disabled;

        let preparation::ResolvedPorts {
            cron_scheduler_concrete,
            mcp_pool_concrete,
            mcp_agent_registry,
            tool_search_index_concrete,
            workflow_middleware_concrete,
            auto_classifier,
            effective_broker,
        } = preparation::resolve_ports(ctx);

        // AskUser 工具（2026-08-15 拆分后）由链上 HumanInTheLoopMiddleware
        // 的 collect_tools 提供（使用原始 broker 而非 MultiplexBroker——
        // ChannelBroker 对 Questions 立即返回空答案、MultiplexBroker 竞速时
        // Channel 总是先返回，导致 AskUserQuestion 弹窗被绕过）；宿主级
        // shared_tools 不再注册任何工具。

        let parent_tools = preparation::build_parent_tools(ctx, &mcp_pool_concrete);

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
                    prompt::add_agents_md(ctx, &mut chain);
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
                    prompt::add_skills(ctx, &mut chain);
                }
                ChainSlot::SkillPreload if disabled.contains("SkillPreloadMiddleware") => {}
                ChainSlot::SkillPreload => {
                    prompt::add_skill_preload(ctx, &mut chain);
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
                    hooks::add_hooks(ctx, &mut chain);
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
                    mcp::add_mcp(ctx, &mut chain, &mcp_pool_concrete);
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
                    lsp::add_lsp(ctx, &mut chain);
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

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
