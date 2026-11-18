use peri_acp_types::meta_harness::{AssemblyPlan, ChainSlot, ExecutionProfile};

use super::{
    mpsc, AgentsMdMiddleware, Arc, McpSkillRegistry, Middleware, MiddlewareChain,
    SkillPreloadMiddleware, SkillsMiddleware, SubAgentMiddlewareConfig, SubagentChainAssembler,
    SubagentChainContext, TodoMiddleware,
};
use std::fmt;
use std::fmt::{Debug, Formatter};

/// 构造 SubAgent 标准中间件链
///
/// ## 与主 Agent 中间件链的差异（P1-13）
///
/// 以下中间件**有意不在 SubAgent 链中注册**：
///
/// | 中间件 | 省略原因 |
/// |--------|----------|
/// | `GitAttributionMiddleware` | SubAgent 工具调用无需 git 贡献追踪 |
/// | `AtMentionMiddleware` | @path 解析仅在主 Agent 用户交互中生效 |
/// | `PluginMiddleware` | 插件仅在主 Agent 中加载 |
/// | `HITLMiddleware` | SubAgent 工具执行沿用父 Agent 的审批模式 |
///
/// `CronMiddleware` 不在此表：v4 起 cron 不再是链槽位，能力由 builtin `cron`
/// 实例提供，`CronMiddleware` 只作为该实例的策略键（`BUILTIN_INSTANCE_POLICY_KEYS`）；
/// SubAgent 不创建 MCP 连接；工具继承父 Reason 的会话目录，deferred 工具
/// 由子链 ToolSearch 提供发现和执行入口。
///
/// 以下中间件通过**参数注入**方式支持 SubAgent：
///
/// | 中间件 | 注入方式 |
/// |--------|----------|
/// | `Hook` (生命周期) | 经 `HookDispatcher::fire_subagent_lifecycle()` 分发（默认非阻断） |
#[must_use]
pub fn build_subagent_middlewares(config: SubAgentMiddlewareConfig) -> Vec<Box<dyn Middleware>> {
    let mut middlewares: Vec<Box<dyn Middleware>> = Vec::new();

    // MetaHarness（设计 §2.5）：子链独立装配，关闭面同样生效——
    // 关闭的 middleware 不构造、不进链（构造副作用也不存在）。
    let plan = AssemblyPlan::new(&config.meta_harness_disabled, ExecutionProfile::Child);

    // [TRAP] SubAgent 复用 main agent 在 session/new 时捕获的 frozen CLAUDE.md，
    // 避免文件中途变更导致 system prompt 漂移（第一优先级不变量）。
    if plan.contains(ChainSlot::AgentsMd) {
        // M4：main / local 独立贡献（local-only 有效）。
        let agents_md = AgentsMdMiddleware::new()
            .with_frozen_parts(config.frozen_claude_md, config.frozen_claude_local_md);
        middlewares.push(Box::new(agents_md));
    }

    // [TRAP] 同上：SubAgent 复用 frozen skill summary。
    // W4b（F5/J5）：子链的目录与正文都只来自 MCP registry（`config.mcp_skill_registry`
    // 由装配面注入父会话的会话级 registry），无本地扫描、无磁盘回落。
    if plan.contains(ChainSlot::Skills) {
        let mut skills =
            SkillsMiddleware::new().with_mcp_registry(config.mcp_skill_registry.clone());
        if let Some(summary) = config.frozen_skill_summary {
            skills = skills.with_frozen_summary(summary);
        }
        middlewares.push(Box::new(skills));
    }

    if !config.skill_names.is_empty() && plan.contains(ChainSlot::SkillPreload) {
        middlewares.push(Box::new(
            SkillPreloadMiddleware::new(config.skill_names)
                .with_mcp_registry(config.mcp_skill_registry.clone()),
        ));
    }
    if plan.contains(ChainSlot::Todo) {
        middlewares.push(Box::new(TodoMiddleware::new({
            let (tx, _rx) = mpsc::channel(8);
            tx
        })));
    }
    if plan.contains(ChainSlot::ToolSearch) {
        middlewares.push(Box::new(crate::tool_search::ToolSearchMiddleware::new(
            Arc::new(crate::tool_search::ToolSearchIndex::new()),
            Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::new())),
        )));
    }
    middlewares
}

/// 子 agent 链装配器实现（L3）：经 [`SubagentChainAssembler`] trait 依赖反转，
/// 由 middlewares 提供实现——Agent 层 [`SessionFactory::spawn_subagent`](peri_agent::session::subagent::SessionFactory::spawn_subagent) 从父 session copy frozen
/// 数据后调用本实现构建子链，链序保持 [`build_subagent_middlewares`] 不变
/// （AgentsMd→Skills→`[SkillPreload]`→Todo→`[ToolSearch]`，ARC-MIDDLEWARE-001）。
///
/// W4b（F5/J5）：装配器持有会话级 MCP skill registry（父链装配面注入），子链的
/// 技能目录/正文因此只有 MCP 一个来源；未装配时子链无技能面，不回落磁盘。
pub struct SubagentChainAssemblerImpl {
    pub(super) mcp_skill_registry: Option<Arc<McpSkillRegistry>>,
}

impl Debug for SubagentChainAssemblerImpl {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubagentChainAssemblerImpl")
            .finish_non_exhaustive()
    }
}

impl SubagentChainAssemblerImpl {
    /// 无技能面装配器（registry 未注入：子链只报缺口）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            mcp_skill_registry: None,
        }
    }

    /// 注入会话级 MCP skill registry（父链装配面调用）。
    #[must_use]
    pub fn with_registry(registry: Option<Arc<McpSkillRegistry>>) -> Self {
        Self {
            mcp_skill_registry: registry,
        }
    }
}

impl Default for SubagentChainAssemblerImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl SubagentChainAssembler for SubagentChainAssemblerImpl {
    fn assemble(&self, ctx: &SubagentChainContext) -> MiddlewareChain {
        let config = super::super::SubAgentMiddlewareConfig::for_agent_def(
            ctx.skill_names.clone(),
            &ctx.cwd,
        )
        .with_frozen(
            ctx.frozen_claude_md.clone(),
            ctx.frozen_claude_local_md.clone(),
            ctx.frozen_skill_summary.clone(),
        )
        .with_mcp_registry(self.mcp_skill_registry.clone())
        .with_meta_harness_disabled(ctx.meta_harness_disabled.clone());
        let mut chain = MiddlewareChain::new();
        for mw in build_subagent_middlewares(config) {
            chain.add(mw);
        }
        chain
    }
}
