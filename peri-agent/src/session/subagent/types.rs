use std::collections::HashSet;
use std::sync::Arc;

use peri_acp_types::identity::AgentId;
use peri_acp_types::thread::CancelPolicy;
use tokio_util::sync::CancellationToken;

use crate::agent::async_tasks::{BgTaskKind, TaskManager};
use crate::agent::events::{AgentEventHandler, ExecutorEvent};
use crate::agent::react::ReactLLM;
use crate::agent::{CompactConfig, ContextBudget, LangfuseBridgeLike};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::messages::BaseMessage;
use crate::middleware::chain::MiddlewareChain;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::session::Session;
use crate::thread::ThreadStore;
use crate::tools::{BaseTool, ToolInvocationResolver};

// ─── 意图类型 ────────────────────────────────────────────────────────────────

/// Fork 指令类型，决定 fork agent 使用的 system directive 模板
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkDirectiveKind {
    /// 使用 [`build_fork_directive`]（英文，Agent 工具路径）
    Fork,
    /// 使用 [`build_bg_fork_directive`]（中文，/bg 命令路径）
    Bg,
}

/// subagent 取消策略（与 ThreadMeta.cancel_policy 强类型对齐）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentCancelPolicy {
    /// Parent cancel → child cancel（同步 fork / 同步 agent 定义）
    Cascade,
    /// 仅 session 级 cancel_all_agents 可停止（后台）
    Independent,
}

impl SubagentCancelPolicy {
    pub(super) fn as_cancel_policy(self) -> CancelPolicy {
        match self {
            Self::Cascade => CancelPolicy::Cascade,
            Self::Independent => CancelPolicy::Independent,
        }
    }
}

/// 运行模式：同步（当前 turn 内跑完）或后台（tokio::spawn + TaskManager 注册）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentRunMode {
    Sync,
    Background,
}

/// 子 agent 生命周期 hook 触发闭包（middlewares 构造，内部触发 RegisteredHook）。
/// 参数：(agent_name, cwd)。
pub type SubagentLifecycleStart = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// 参数：(agent_name, cwd, result, is_error)。
pub type SubagentLifecycleStop = Arc<dyn Fn(&str, &str, &str, bool) + Send + Sync>;

// ─── 子链装配（依赖反转，ARC-MIDDLEWARE-001） ───────────────────────────────

/// 子 agent 链装配上下文：frozen 数据由 [`spawn_subagent`] 从父 session copy 后注入。
#[derive(Debug, Clone, Default)]
pub struct SubagentChainContext {
    /// 工作目录（解析 skill 文件路径）
    pub cwd: String,
    /// 需要预加载的 skill 名称（空 = 跳过 SkillPreloadMiddleware）
    pub skill_names: Vec<String>,
    /// Frozen CLAUDE.md/AGENTS.md main content（父 session copy；None = 从磁盘读取）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（上层注入的冻结数据）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（父 session copy）
    pub frozen_skill_summary: Option<String>,
    /// 装配期关闭的 middleware 名集合（父会话冻结状态投影；
    /// 子链独立装配，必须同样过滤——设计 §2.5）。
    pub meta_harness_disabled: HashSet<String>,
}

/// 子 agent 中间件链装配器：由中间件层提供实现。
///
/// 链序（AgentsMd→Skills→[SkillPreload]→Todo）是行为契约，实现方必须保持
/// `peri-middlewares/src/subagent/tool/mod.rs` 的 `build_subagent_middlewares`
/// 顺序（ARC-MIDDLEWARE-001）。
pub trait SubagentChainAssembler: Send + Sync {
    fn assemble(&self, ctx: &SubagentChainContext) -> MiddlewareChain;
}

// ─── 父侧运行时宿主 ──────────────────────────────────────────────────────────

/// 父侧运行时通道聚合（L3）：executor/builder 在主 session 创建后注入，
/// subagent 创建所需的运行时通道统一经此读取，不再逐字段透传
/// SubAgentMiddleware。
#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct SubagentHost {
    /// 线程持久化存储（生产路径非 None；None 仅测试/遗留路径，跳过落库）
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 后台任务管理器（per-session 聚合）
    pub task_manager: Option<Arc<TaskManager>>,
    /// 后台任务完成事件通道（bg pump，独立于主 event pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// bg 完成同步回调（registry.complete 之前调用，推送 Defer 到主 agent MQ）
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// 子 agent 启动注册回调（active_agents）
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// Langfuse bridge（subagent trace）
    pub langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    /// Frozen CLAUDE.local.md（父 session 冻结数据中唯一不在 FrozenContext 的字段）
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建；父 session 冻结的 subagent 版本）
    pub frozen_system_prompt: Option<Arc<String>>,
    /// 父线程 ID 回退值：被 [`parent_thread_id_of`] 在 spawn 写盘读取
    /// （主 session `store().thread_id` 恒 None 时是本链的权威值，由 executor
    /// 以 `ctx.thread_id` 注入；生产路径为 spawn_subagent 从 parent session
    /// 读取的同一回退源）
    pub parent_thread_id: Option<String>,
    /// Frozen CLAUDE.md 回退值（生产路径由 spawn_subagent 从 parent session copy）
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen skills summary 回退值（生产路径由 spawn_subagent 从 parent session copy）
    pub frozen_skill_summary: Option<Arc<String>>,
    /// Session-local Dynamic MCP capability publisher shared by the whole agent tree.
    pub session_mcp_capability: Option<Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort>>,
}

// ─── spawn 配置与产物 ────────────────────────────────────────────────────────

/// 子 agent 创建意图 + 装配产物 + 运行时通道（统一入口 [`spawn_subagent`] 的输入）。
///
/// 父侧数据（cwd / parent_thread_id / frozen claude_md / skill_summary / date /
/// cascade cancel token）在 `parent` 存在时从 parent Session 读取，config 中
/// 对应字段仅作 parent 缺失（/bg 命令等无 session 路径）时的回退。
#[allow(clippy::type_complexity)]
pub struct SubagentSpawnConfig {
    // ── 意图 ──
    /// 子 agent 名（事件 agent_name / thread title / task agent_name）
    pub agent_name: String,
    /// 派发给子 Agent 的任务描述（fork 路径经 fork directive 包装后入队）
    pub prompt: String,
    /// 父会话消息历史（fork 路径注入 transcript 让子 agent 理解上下文）
    pub parent_messages: Vec<BaseMessage>,
    /// 取消策略（Cascade = 父 cancel 传播，Independent = 独立）
    pub cancel_policy: SubagentCancelPolicy,
    /// 最大 ReAct 迭代次数
    pub max_iterations: usize,
    /// fork directive 模板（None = 不包装，直接 push prompt——agent 定义路径）
    pub fork_directive_kind: Option<ForkDirectiveKind>,
    /// 运行模式
    pub run_mode: SubagentRunMode,
    /// agent 定义声明的 skills（SkillPreload 装配输入）
    pub skill_names: Vec<String>,
    // ── 装配产物 ──
    /// SubAgent LLM（ReactLLM 实现/装饰器）
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    /// 子 agent 中间件链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub chain_assembler: Arc<dyn SubagentChainAssembler>,
    /// 过滤后的工具集（agent 定义路径按 tools/disallowed_tools 过滤）
    pub tools: Vec<Arc<dyn BaseTool>>,
    /// Canonical child policy, reapplied after every capability generation refresh.
    pub tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// SubAgent system prompt（注入 transcript 起始处）
    pub system_prompt: Option<String>,
    /// 错误感知建议注册表（可选）
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（None 用 default）
    pub tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    /// deferred 工具解析器（None = DirectToolInvocationResolver；middlewares 传
    /// ExecuteExtraToolResolver 保持包装层语义）
    pub tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    /// auto-compact 阈值配置（None = 不启用）
    pub compact_config: Option<CompactConfig>,
    /// 上下文预算（None = 不追踪 token 使用率）
    pub context_budget: Option<ContextBudget>,
    /// Full Compact 专用 LLM（None 时 Full Compact 跳过）
    pub compact_llm: Option<Arc<dyn peri_model::Model>>,
    // ── 运行时通道 ──
    /// 线程持久化存储（None = 不落库，仅测试/遗留路径）
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 父 agent 事件 handler（同步路径事件转发 / 重试事件追踪）
    pub event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// bg 任务完成事件发送通道（bg pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// 后台任务管理器（Background 模式必填）
    pub task_manager: Option<Arc<TaskManager>>,
    /// bg 完成同步回调
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// Langfuse bridge
    pub langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_start: Option<SubagentLifecycleStart>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_stop: Option<SubagentLifecycleStop>,
    /// 子 agent 启动注册回调
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// 父 agent 事件侧 AgentId（v2 SubagentStart/Stop 的 agent_id 字段；
    /// None = /bg 命令等无 Langfuse tracer 路径 → 不 emit v2 Start/Stop）
    pub parent_agent_id: Option<AgentId>,
    // ── 父侧数据回退（parent 为 None 时使用；parent 存在时被覆盖） ──
    /// 父 cancel token（Cascade 时取其 child_token；parent 存在时从 parent 读取）
    pub cancel_token: Option<CancellationToken>,
    /// 工作目录
    pub cwd: Option<String>,
    /// 父线程 ID
    pub parent_thread_id: Option<String>,
    /// Frozen CLAUDE.md main content（回退值）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（父 session 无此字段，恒由上层注入）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（回退值）
    pub frozen_skill_summary: Option<String>,
    /// Frozen 日期 YYYY-MM-DD（回退值）
    pub frozen_date: Option<String>,
}

/// spawn 产物
pub struct SubagentSpawned {
    /// 子线程 ID（= 子 session thread_id = 身份键来源）
    pub child_thread_id: String,
    /// 后台任务 ID（仅 Background 模式；格式 bg-{uuid v7}）
    pub task_id: Option<String>,
    /// 子 session（调用方读取 transcript）
    pub session: Arc<Session>,
    /// 生成的 cancel token（Background 注册 / 返回消息使用）
    pub cancel_token: CancellationToken,
    /// 是否被中断（Sync 模式；Background 模式恒 false）
    pub interrupted: bool,
}

// ─── resume 配置（统一恢复入口 [`resume_subagent`] 的输入） ─────────────────

/// 子 agent 恢复意图 + 装配产物 + 运行时通道（[`SessionFactory::resume_subagent`] 的输入）。
///
/// 与 [`SubagentSpawnConfig`] 的字段差异（恢复语义禁止项，不提供）：
/// - 无 `parent_messages` / `system_prompt` / `fork_directive_kind`（F4：已在旧
///   transcript 中，重复注入会重复）；
/// - 无 `skill_names`（R-H1：SkillPreload 重复注入——旧 transcript 已含首轮注入
///   的 skill 内容，恢复时恒传空）；
/// - `thread_store` 必填（恢复现场的唯一来源是磁盘 thread）。
///
/// 父侧数据（cwd / parent_thread_id / frozen 回退值）在 `parent` 存在时从 parent
/// Session 读取，config 中对应字段仅作 parent 缺失时的回退（与 spawn 一致）。
#[allow(clippy::type_complexity)]
pub struct SubagentResumeConfig {
    // ── 意图 ──
    /// 要恢复的子线程 ID（thread_id 不变，可无限次恢复重入）
    pub thread_id: String,
    /// 追加指令（None = 隐式 continue，slice 4 处理）
    pub prompt: Option<String>,
    /// 子 agent 名（None 时从 meta.title 取）
    pub agent_name: Option<String>,
    /// 运行模式（恢复时由本次调用决定，issue 决策 8）
    pub run_mode: SubagentRunMode,
    /// 最大 ReAct 迭代次数
    pub max_iterations: usize,
    // ── 装配产物 ──
    /// SubAgent LLM（ReactLLM 实现/装饰器）
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    /// 子 agent 中间件链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub chain_assembler: Arc<dyn SubagentChainAssembler>,
    /// 过滤后的工具集（恢复路径由 tool 层按 title 重新应用过滤）
    pub tools: Vec<Arc<dyn BaseTool>>,
    /// Canonical child policy, reapplied after every capability generation refresh.
    pub tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// deferred 工具解析器（None = DirectToolInvocationResolver；middlewares 传
    /// ExecuteExtraToolResolver 保持包装层语义）
    pub tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    /// 错误感知建议注册表（可选）
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（None 用 default）
    pub tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    /// auto-compact 阈值配置（None = 不启用）
    pub compact_config: Option<CompactConfig>,
    /// 上下文预算（None = 不追踪 token 使用率）
    pub context_budget: Option<ContextBudget>,
    /// Full Compact 专用 LLM（None 时 Full Compact 跳过）
    pub compact_llm: Option<Arc<dyn peri_model::Model>>,
    // ── 运行时通道 ──
    /// 线程持久化存储（必填：恢复现场来源）
    pub thread_store: Arc<dyn ThreadStore>,
    /// 父 agent 事件 handler（同步路径事件转发 / 重试事件追踪）
    pub event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// bg 任务完成事件发送通道（bg pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// 后台任务管理器（Background 模式必填）
    pub task_manager: Option<Arc<TaskManager>>,
    /// bg 完成同步回调
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// Langfuse bridge
    pub langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_start: Option<SubagentLifecycleStart>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_stop: Option<SubagentLifecycleStop>,
    /// 子 agent 启动注册回调
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// 父 agent 事件侧 AgentId（v2 SubagentStart/Stop 的 agent_id 字段；
    /// None = /bg 命令等无 Langfuse tracer 路径 → 不 emit v2 Start/Stop）
    pub parent_agent_id: Option<AgentId>,
    // ── 父侧数据回退（parent 为 None 时使用；parent 存在时被覆盖） ──
    /// 父 cancel token（Cascade 时取其 child_token；parent 存在时从 parent 读取）
    pub cancel_token: Option<CancellationToken>,
    /// 工作目录
    pub cwd: Option<String>,
    /// Frozen CLAUDE.md main content（回退值）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（父 session 无此字段，恒由上层注入）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（回退值）
    pub frozen_skill_summary: Option<String>,
    /// Frozen 日期 YYYY-MM-DD（回退值）
    pub frozen_date: Option<String>,
}
