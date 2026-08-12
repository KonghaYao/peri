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

// ── 装配上下文（L5：自 peri-middlewares/src/assembly.rs 迁入）────────────────
//
// 链装配上下文与产物的类型事实源归本层（§2 装配归 Agent 层 session 工厂）：
// 装配实现方（`peri-middlewares::assembly::ProductionChainAssembler`）依赖
// 本层类型，避免 Agent 层反向依赖 middlewares 成环。middlewares 具体类型
// 全部经 `peri-acp-types` 端口（McpPoolPort / ToolSearchPort /
// WorkflowMiddlewarePort / CronSchedulerPort）或注入面接入。

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::RwLock;

use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::event::AgentEventHandler;
use peri_acp_types::goal::GoalController;
use peri_acp_types::hooks::RegisteredHook;
use peri_acp_types::interaction::{ChannelState, UserInteractionBroker};
use peri_acp_types::lsp::LspServerConfig;
use peri_acp_types::plugin::LoadedPlugin;
use peri_acp_types::ports::{LspPoolPort, McpPoolPort, ToolSearchPort, WorkflowMiddlewarePort};
use peri_acp_types::skills::SkillRoot;
use peri_acp_types::store::ThreadStore;
use peri_acp_types::tools::TodoItem;
use peri_acp_types::workflow::AgentExecutor;
use peri_acp_types::{identity::AgentId, permission::SharedPermissionMode};

use crate::agent::async_tasks::{BgTaskKind, TaskManager};
use crate::agent::events::BackgroundTaskResult;
use crate::agent::react::ReactLLM;
use crate::agent::LangfuseBridgeLike;
use crate::agent::{AgentCancellationToken, ExecutorEvent};
use crate::middleware::chain::MiddlewareChain;
use crate::session::Session;
use crate::tools::BaseTool;

/// 后台任务完成回调类型（第二参为任务 kind，供 continuation scheduler 过滤）。
pub type OnBgCompleteFn = Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>;
/// System prompt 构建器类型。
pub type SystemPromptBuilder = Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>;

/// SubAgent 中间件端口（L5：装配产物经本端口向主 session 注入父身份）。
///
/// 实现方为 `peri-middlewares::subagent::SubAgentMiddleware`（middlewares →
/// Agent 声明边）；stage 装配（Agent 层）只经本端口调用，不触碰具体实现。
pub trait SubAgentMiddlewarePort: Send + Sync {
    /// 还原具体实现（装配方 downcast 用）。
    fn as_any(&self) -> &dyn Any;
    /// 注入父 agent 事件侧 AgentId（主 v2 session 创建后调用）。
    fn set_parent_agent_id(&self, id: AgentId);
    /// 注入父 v2 session（L3，主 session 创建后调用）。
    fn set_parent_session(&self, session: Arc<Session>);
}

impl dyn SubAgentMiddlewarePort {
    /// 将 `Arc<dyn SubAgentMiddlewarePort>` 还原为具体实现 `Arc<T>`。
    pub fn downcast_arc<T: SubAgentMiddlewarePort + 'static>(
        self: Arc<Self>,
    ) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn SubAgentMiddlewarePort>()`（trait object
            // 自身），恒不等于 `TypeId::of::<T>()` → downcast 恒失败
            // （同构 2026-08-06-e2e-workflow-not-completing 遗留项）。
            if (*ptr).as_any().type_id() == std::any::TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// 链装配上下文（Agent 层装配接口的上下文投影；L5 自
/// `peri-middlewares::assembly::AssemblyContext` 迁入）。
///
/// 由 stage 装配（`session::exec::stage_builder`）从会话输入投影构造，
/// 仅含中间件构造所需的依赖；middlewares 具体类型经
/// `peri-acp-types` 端口（`McpPoolPort` / `ToolSearchPort` /
/// `WorkflowMiddlewarePort` / `CronSchedulerPort`）接入，
/// 装配实现方（`ProductionChainAssembler`）内部 downcast 还原。
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
    /// Cron 调度器端口（None = 构造临时实例；装配方 downcast 还原）
    pub cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    /// MCP 连接池端口（None = 不注册 MCP 中间件/工具）
    pub mcp_pool: Option<Arc<dyn McpPoolPort>>,
    /// Channel 状态（MultiplexBroker 包装用）
    pub channel_state: Option<Arc<ChannelState>>,
    /// 工具搜索索引端口
    pub tool_search_index: Arc<dyn ToolSearchPort>,
    /// 共享工具注册表（deferred tools；AskUserTool 插入、snapshot 构造）
    pub shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    pub lsp_servers: Vec<LspServerConfig>,
    /// 会话级 LSP 服务器池端口（复用，None = 构造临时实例；装配方 downcast 还原）
    pub lsp_pool: Option<Arc<dyn LspPoolPort>>,
    /// Workflow executor（Some 时注册 Workflow 中间件）
    pub workflow_executor: Option<Arc<dyn AgentExecutor>>,
    /// 会话级 WorkflowMiddleware 端口（复用，None = 构造临时实例）
    pub workflow_middleware: Option<Arc<dyn WorkflowMiddlewarePort>>,
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

/// 链装配产物（stage 装配直接消费）。
pub struct ChainAssembly {
    /// 中间件链（StageContext 复用）
    pub chain: MiddlewareChain,
    /// SubAgent 中间件端口（链中已有一份 clone；供上层注入主 agent 身份）
    pub subagent_mw: Option<Arc<dyn SubAgentMiddlewarePort>>,
    /// 错误感知建议注册表
    pub error_suggest_registry: Option<Arc<crate::error_suggest::ErrorSuggestRegistry>>,
    /// 工具注册表快照（工具名 + subagent 类型）
    pub tool_registry_snapshot: Arc<crate::error_suggest::ToolRegistrySnapshot>,
}
