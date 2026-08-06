//! Agent 层 session 工厂 —— session 初始化时的装配入口。
//!
//! 3.0 归位（L2）：中间件链装配的链序事实源随本模块从
//! `peri-acp/src/agent/builder.rs` 迁入（ARC-MIDDLEWARE-001 同步迁）。
//! 具体中间件实例的构造由 [`MiddlewareChainAssembler`] 实现方提供——
//! 当前唯一实现为 `peri-middlewares::assembly::ProductionChainAssembler`
//! （中间件实现依赖本层 trait，避免 Agent 层反向依赖 middlewares 成环）；
//! 依赖反转（中间件类型下沉）完成后，装配实现将物理迁入本层。
//!
//! 会话级冻结数据（[`FrozenData`]）与子 Agent 线程持久化（[`ThreadPersistence`]）
//! 亦自 ACP builder 随迁至此，保持构建入口的归位。

/// 子 Agent 事件 handler 工厂类型（事实源 peri-acp-types::frozen）
pub use peri_acp_types::frozen::ChildHandlerFactory;

/// Register/Deregister 回调与冻结数据（事实源 peri-acp-types::frozen）
pub use peri_acp_types::frozen::{
    DeregisterRuntimeFn, FrozenData, RegisterRuntimeFn, ThreadPersistence,
};

/// 生产链槽位（顺序 = 行为契约，ARC-MIDDLEWARE-001，禁止重排）。
///
/// 顺序与迁移前 `peri-acp/src/agent/builder.rs` 的 `MiddlewareChain`
/// 构造顺序完全一致，按功能分组；条件注册（MCP/Workflow/LSP/Goal）与
/// Hook 组展开由装配实现按上下文判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainSlot {
    // ── 第一组：上下文注入器（system prompt 段落 / agent 定义 / 插件 / skills） ──
    /// AgentsMd（CLAUDE.md 指引注入）
    AgentsMd,
    /// AgentDefine（agent 定义注入）
    AgentDefine,
    /// Plugin（插件加载结果注入）
    Plugin,
    /// Skills（技能摘要注入）
    Skills,
    /// SkillPreload（预加载技能工具）
    SkillPreload,
    /// AtMention（@mention 解析）
    AtMention,
    /// Image（@image 附件转 ContentBlock::Image）
    Image,
    // ── 第二组：文件/终端/Web 工具提供器 ──
    /// Filesystem（文件系统工具）
    Filesystem,
    /// GitAttribution（git 归属注入）
    GitAttribution,
    /// Terminal（终端命令工具）
    Terminal,
    /// Web（Web 工具）
    Web,
    // ── 第三组：Todo / Cron ──
    /// Todo（todo 工具）
    Todo,
    /// Cron（cron 工具）
    Cron,
    // ── 第四组：Hook 中间件（插件 hooks + 自定义 hooks） ──
    /// Hook 哨兵：每个非空 hook group 展开一个 HookMiddleware 实例
    Hook,
    // ── 第五组：HITL + SubAgent（条件中间件） ──
    /// Hitl（人类在环审批）
    Hitl,
    /// SubAgent（子 Agent 工具）
    SubAgent,
    // ── 第六组：MCP / Workflow / ToolSearch（工具提供器，条件注册） ──
    /// Mcp（MCP 工具，pool 可用时注册）
    Mcp,
    /// Workflow（workflow 工具，executor 可用时注册）
    Workflow,
    /// ToolSearch（deferred 工具搜索/执行代理）
    ToolSearch,
    // ── 第七组：LSP / Goal（辅助诊断，条件注册；Goal 在链最后） ──
    /// Lsp（LSP 诊断工具，servers 非空时注册）
    Lsp,
    /// Goal（goal 紧迫感 steering，controller 可用时注册）
    Goal,
}

/// 生产链蓝本：槽位顺序 = 链序事实源（ARC-MIDDLEWARE-001）。
///
/// 迁移自 `peri-acp/src/agent/builder.rs` 的 `MiddlewareChain` 构造顺序，
/// 是行为契约，不得按名称/便利性/局部需求重排。
pub fn production_blueprint() -> Vec<ChainSlot> {
    vec![
        // 第一组：上下文注入器
        ChainSlot::AgentsMd,
        ChainSlot::AgentDefine,
        ChainSlot::Plugin,
        ChainSlot::Skills,
        ChainSlot::SkillPreload,
        ChainSlot::AtMention,
        ChainSlot::Image,
        // 第二组：文件/终端/Web 工具提供器
        ChainSlot::Filesystem,
        ChainSlot::GitAttribution,
        ChainSlot::Terminal,
        ChainSlot::Web,
        // 第三组：Todo / Cron
        ChainSlot::Todo,
        ChainSlot::Cron,
        // 第四组：Hook 中间件
        ChainSlot::Hook,
        // 第五组：HITL + SubAgent
        ChainSlot::Hitl,
        ChainSlot::SubAgent,
        // 第六组：MCP / Workflow / ToolSearch
        ChainSlot::Mcp,
        ChainSlot::Workflow,
        ChainSlot::ToolSearch,
        // 第七组：LSP / Goal
        ChainSlot::Lsp,
        ChainSlot::Goal,
    ]
}

/// 链装配器：由中间件层提供实现。
///
/// 当前唯一实现为 `peri-middlewares::assembly::ProductionChainAssembler`
/// （中间件实现依赖本层 trait，Agent 层不反向依赖 middlewares）。
/// 依赖反转完成后装配实现将物理迁入本层。
pub trait MiddlewareChainAssembler: Send + Sync {
    /// 装配上下文（由实现方定义，本层不解释具体字段）
    type Context: Send + Sync;
    /// 装配产物（由实现方定义）
    type Output;
    /// 按生产链序构建中间件链
    fn assemble(&self, blueprint: &[ChainSlot], ctx: &Self::Context) -> Self::Output;
}

/// session 初始化装配入口：按生产链序构建中间件链（唯一触发点，ARC-MIDDLEWARE-001）。
///
/// 装配一律经本函数触发（L2：调用点自 `peri-acp/src/agent/builder.rs` 收敛至此，
/// 装配实现经 [`MiddlewareChainAssembler`] trait 边界由中间件层注入），
/// 链序由 [`production_blueprint`] 蓝本保证。
pub fn build_middleware_chain<A: MiddlewareChainAssembler>(
    assembler: &A,
    ctx: &A::Context,
) -> A::Output {
    assembler.assemble(&production_blueprint(), ctx)
}
