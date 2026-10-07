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
mod mcp;
mod preparation;
mod prompt;
#[cfg(not(target_os = "emscripten"))]
mod workflow;
#[cfg(target_os = "emscripten")]
#[path = "assembly/workflow_wasm.rs"]
mod workflow;

// builtin 实例上下文（IF-P3-04 / A33）：宿主装配（`peri-acp`）经这里拿到宿主构造的上下文
// 和 cron 输入；`WorkspaceInstanceInput` 由 `peri-mcp-workspace` 直接公开，宿主直接依赖
// 该能力包，并在 `McpClientPool::run_initialize` 之前经 `with_workspace` 注入。
pub use crate::mcp::builtin::context::{
    BuiltinContextError, BuiltinInstanceContext, CronInstanceInput,
};
pub use workflow::{
    default_workflow_middleware_factory, default_workflow_middleware_factory_with_pool,
    WorkflowAgentMiddlewareFactory,
};

use crate::{
    default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware},
    hitl::HumanInTheLoopMiddleware,
    middleware::TodoMiddleware,
    permission::{default_requires_approval, PermissionMiddleware},
    plugin::PluginMiddleware,
    subagent::SubAgentMiddleware,
    tool_search::ToolSearchMiddleware,
    workflow::{WorkflowMiddleware, WorkflowMiddlewareAdaptor},
    AtMentionMiddleware, GitAttributionMiddleware, GoalMiddleware, ImageMiddleware,
};
use parking_lot::RwLock;
use peri_agent::{
    agent::events::AgentEventHandler,
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    session::factory::{ChainSlot, MiddlewareChainAssembler, SubAgentMiddlewarePort},
    tools::BaseTool,
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

/// `SubAgentMiddleware` 链槽关闭键（A24 关闭集的 MetaHarness 键之一）。
///
/// **单一事实源**（W5）：链装配的跳过判据、Agent registry 的本地面关闭位
/// （`McpAgentRegistry::for_session` 内派生）与宿主装配的派生都引用本常量，
/// 不在别处第三次写字面量。
pub const SUB_AGENT_FACE_CLOSED_KEY: &str = "SubAgentMiddleware";

/// A24 关闭集：`policy_key ∈ disabled_middlewares` 的实例名（BTreeSet，稳定顺序）。
///
/// **只委托、不复制逻辑**：唯一实现是 `crate::mcp::builtin::closed_instances`，本函数让
/// 宿主装配经公开面派生关闭集（随 [`BuiltinInstanceContext`] 一起注入），不必 import
/// `peri_middlewares::mcp::builtin`。
pub fn builtin_closed_instances(
    disabled_middlewares: &std::collections::HashSet<String>,
) -> std::collections::BTreeSet<String> {
    crate::mcp::builtin::closed_instances(disabled_middlewares)
}

/// 生产链装配器（当前唯一装配实现，见模块文档）。
pub struct ProductionChainAssembler;

impl MiddlewareChainAssembler for ProductionChainAssembler {
    type Context = AssemblyContext;
    type Output = ChainAssembly;

    /// 按 Agent 层 `production_blueprint` 的槽位顺序构造中间件链。
    ///
    /// 链序由蓝本保证（ARC-MIDDLEWARE-001 事实源在 Agent 层工厂）；
    /// 本实现只负责逐槽位构造实例，条件注册（MCP/Workflow/Goal）
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
            mcp_pool_concrete,
            mcp_agent_registry,
            tool_search_index_concrete,
            workflow_middleware_concrete,
            auto_classifier,
        } = preparation::resolve_ports(ctx);

        // AskUser 工具（2026-08-15 拆分后）由链上 HumanInTheLoopMiddleware
        // 的 collect_tools 提供；宿主级 shared_tools 不再注册任何工具。

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
        let mut subagent: Option<SubAgentMiddleware> =
            if disabled.contains(SUB_AGENT_FACE_CLOSED_KEY) {
                None
            } else {
                Some(
                    SubAgentMiddleware::new(
                        parent_tools,
                        Some(Arc::clone(event_handler) as Arc<dyn AgentEventHandler>),
                        llm_factory.clone(),
                    )
                    .with_mcp_agents(mcp_agent_registry.clone(), Arc::clone(broker))
                    .with_mcp_skills(ctx.mcp_skill_registry.clone())
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
                // 主 Agent 的 slash token 自动预载属于宿主技能面；关闭该面时
                // 不装配自动预载，显式声明的子任务名单仍按原契约处理。
                ChainSlot::SkillPreload
                    if disabled.contains("SkillsMiddleware") && ctx.preload_skills.is_empty() => {}
                ChainSlot::SkillPreload => {
                    prompt::add_skill_preload(ctx, &mut chain);
                }
                ChainSlot::AtMention if disabled.contains("AtMentionMiddleware") => {}
                ChainSlot::AtMention => {
                    let reader = Arc::new(crate::workspace_io::McpWorkspaceFileReader::new(
                        mcp_pool_concrete.clone(),
                        Some(ctx.session_id.clone()),
                        disabled,
                    ));
                    chain.add(Box::new(AtMentionMiddleware::new(reader)));
                }
                // 新增：图片附件处理（在 @mention 之后，将 @image <path> 转换为 ContentBlock::Image）
                ChainSlot::Image if disabled.contains("ImageMiddleware") => {}
                ChainSlot::Image => {
                    let image = match mcp_pool_concrete.as_ref() {
                        Some(pool) => ImageMiddleware::new().with_mcp_pool(
                            Arc::clone(pool),
                            ctx.session_id.clone(),
                            disabled,
                        ),
                        None => ImageMiddleware::new(),
                    };
                    chain.add(Box::new(image));
                }
                // ── 第二组：工作区观察类注入器 ──
                // v4-part-4 W3-C1：原 Filesystem / Terminal 槽位已删除——7 个文件/终端
                // 工具的唯一提供面是 builtin `workspace` 实例的 bridge（模型面使用原名，
                // 经 McpMiddleware 槽位的 `open_builtin_bridges` 进入链）。
                // v4 wave 4：原 GitWatch 槽位已删除——git ref 变化改由 builtin `workspace`
                // 实例的 `workspace://git/ref` 资源 + MCP 2026-07-28 订阅回传（提醒映射
                // 内置在宿主订阅消费侧），本组只剩留名中间件（ARC-MIDDLEWARE-001）。
                ChainSlot::GitAttribution if disabled.contains("GitAttributionMiddleware") => {}
                ChainSlot::GitAttribution => {
                    let reader = Arc::new(crate::workspace_io::McpWorkspaceFileReader::new(
                        mcp_pool_concrete.clone(),
                        Some(ctx.session_id.clone()),
                        disabled,
                    ));
                    chain.add(Box::new(GitAttributionMiddleware::new(model_name, reader)));
                }
                // ── 第三组：Todo ──
                ChainSlot::Todo if disabled.contains("TodoMiddleware") => {}
                ChainSlot::Todo => {
                    chain.add(Box::new(TodoMiddleware::new(todo_tx.clone())));
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
                    chain.add(Box::new(
                        PermissionMiddleware::with_shared_mode(
                            broker.clone(),
                            default_requires_approval,
                            permission_mode.clone(),
                            auto_classifier.clone(),
                        )
                        .with_prompt_disabled_builtin((*disabled).clone()),
                    ));
                }
                ChainSlot::AskUser if disabled.contains("HumanInTheLoopMiddleware") => {}
                ChainSlot::AskUser => {
                    chain.add(Box::new(HumanInTheLoopMiddleware::new(broker.clone())));
                }
                // chain 与上层各持一份 SubAgentMiddleware clone：
                // 链中实例负责 collect_tools 提供 SubAgentTool；原实例由上层
                // 注入主 agent 身份（共享 cell，见 set_parent_agent_id）。
                // MetaHarness：SubAgentMiddleware 关闭 → 链上不注册（subagent_mw
                // 槽位在下方联动置 None）。
                ChainSlot::SubAgent if disabled.contains(SUB_AGENT_FACE_CLOSED_KEY) => {}
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
                // ToolSearch 中间件
                ChainSlot::ToolSearch if disabled.contains("ToolSearch") => {}
                ChainSlot::ToolSearch => {
                    chain.add(Box::new(ToolSearchMiddleware::new(
                        Arc::clone(&tool_search_index_concrete),
                        Arc::clone(shared_tools),
                    )));
                }
                // ── 第七组：Goal（辅助诊断；Goal 链最后） ──
                // MetaHarness：Goal 关闭 → 即使运行条件满足也不构造。
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

        ChainAssembly {
            chain,
            // MetaHarness：SubAgentMiddleware 关闭 → 槽位联动置空（禁止半开状态）。
            subagent_mw: subagent.map(|mw| Arc::new(mw) as Arc<dyn SubAgentMiddlewarePort>),
        }
    }
}

// 装配触发点收敛：不再提供本层便捷入口。装配一律经 Agent 层 session 工厂的
// `build_middleware_chain`（唯一触发点，ARC-MIDDLEWARE-001）触发，
// 本模块仅保留 trait 实现（`ProductionChainAssembler`）。

/// builtin 实例的 **direct** bridge 提供面（A6 面②/③；IF-D10 面②/③）。
///
/// Web / Artifact 能力由 builtin bridge 以原名存在于 MCP 目录：
/// `build_typed_tool_bridges` 应用注册表声明的 direct（IF-D13），本函数再按
/// 同一份 frozen policy 去掉关闭的 builtin 实例（按 bridge 的来源身份判定，
/// 不能按 server 名误关接管同名 Workspace 的远端 MCP）。
///
/// 只保留 direct：workflow agent 没有 ToolSearch；SubAgent 装配时使用本面
/// 作为初始工具集，在父 Reason 发布时再绑定当前会话的完整静态 MCP 目录。
pub(crate) fn open_builtin_bridges(
    pool: &Arc<crate::mcp::McpClientPool>,
    disabled: &std::collections::HashSet<String>,
) -> Vec<Box<dyn BaseTool>> {
    let closed = crate::mcp::builtin::closed_instances(disabled);
    crate::mcp::tool_bridge::build_typed_tool_bridges(pool)
        .into_iter()
        .filter(|bridge| {
            !bridge
                .builtin_mcp_instance()
                .is_some_and(|instance| crate::mcp::builtin::is_closed(instance, &closed))
        })
        .filter(|bridge| bridge.is_direct())
        .map(|bridge| Box::new(bridge) as Box<dyn BaseTool>)
        .collect()
}

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
