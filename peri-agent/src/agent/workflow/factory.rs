//! Workflow agent 装配注入端口（p1-wa 收口）。
//!
//! §0 边 8（Agent 禁入 Middleware）：workflow agent 执行体（`agent.rs`）所需的
//! 中间件链 / 工具列表 / error_suggest / tool resolver 装配全部经本端口参数化，
//! 由实现方（`peri-middlewares`，§0 Middleware → Agent 声明边）构造具体实例，
//! ACP 宿主装配点（`assemble.rs` / `stdio/init.rs`，经 TUI 部署装配点注入）
//! 负责把实现 upcast 为端口后注入 [`crate::agent::workflow::WorkflowAgentContext`]。
//!
//! 参照既有 `TaskManager`（`peri-acp-types::tasks`）/ `MiddlewareChainAssembler`
//! （`crate::session::factory`）注入先例：本模块只声明端口，不持有实现。

use std::sync::Arc;

use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult};

use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::middleware::r#trait::Middleware;
use crate::tools::{BaseTool, ToolInvocationResolver};

use super::WorkflowAgentContext;

/// Workflow agent 从 `agentType` 解析出的 subagent 定义投影。
///
/// 解析、文件优先级和工具实例化保留在 `peri-middlewares`；本结构只携带 Agent
/// 执行单元需要的无行为数据，避免反向依赖 Middleware。
#[derive(Debug, Clone, Default)]
pub struct WorkflowAgentDefinition {
    /// agent.md 指定的模型档位；`None` / `inherit` 表示继承 workflow 请求。
    pub model: Option<String>,
    /// `tools` 的显式白名单。`None` 表示继承 workflow 的基础工具；Some(empty)
    /// 表示严格零工具边界。
    pub allowed_tools: Option<Vec<String>>,
    /// 在白名单/继承结果上继续移除的工具。
    pub disallowed_tools: Vec<String>,
    /// agent.md 声明、需要预加载的 skills。
    pub skill_names: Vec<String>,
    /// agent.md 声明的沙箱写目录；有声明且 tools 非 `[]` 时注入 SandboxWrite。
    pub allowed_write_dirs: Vec<String>,
    /// agent.md 的 ReAct 最大轮数；0 表示使用默认值。
    pub max_iterations: usize,
    /// agent.md 对标准 subagent prompt 的 persona/tone/proactiveness 覆盖。
    pub prompt_overrides: Option<AgentOverrides>,
}

/// Workflow agent 中间件/工具装配端口。
///
/// `peri-middlewares` 实现（`assembly::WorkflowAgentMiddlewareFactory`）；
/// 方法面 = workflow agent 执行体所需的全部中间件/工具装配，返回类型一律为
/// Agent 层/契约层类型（实现方经 re-export 构造，不产生 ACP/Middleware 依赖）。
pub trait WorkflowMiddlewareFactory: Send + Sync {
    /// 按普通 subagent 的同一优先级解析 `agentType`。
    fn resolve_agent_definition(
        &self,
        agent_type: &str,
        cwd: &str,
    ) -> Result<WorkflowAgentDefinition, String>;

    /// 装配 workflow agent 工具列表（filesystem / terminal / web / skills tools；
    /// 仅 project-level skills，与迁移前行为一致）。
    fn build_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>>;

    /// 为 agent.md 的 `allowedWriteDirs` 创建最小权限的 SandboxWrite 工具。
    /// 定义中的 `tools: []` / `disallowedTools` 边界由调用方先行检查。
    fn build_sandbox_write_tool(
        &self,
        cwd: &str,
        allowed_dirs: &[String],
    ) -> Option<Box<dyn BaseTool>>;

    /// 装配 workflow agent 中间件链（frozen CLAUDE.md / skills summary / HITL
    /// broker+permission_mode 语义自 `ctx` 读取；`model_name` 供
    /// GitAttribution 使用——alias 解析后的有效模型名）。
    fn build_middlewares(
        &self,
        ctx: &WorkflowAgentContext,
        model_name: &str,
        skill_names: &[String],
    ) -> Vec<Box<dyn Middleware>>;

    /// 构造 tool invocation resolver（迁移前语义 =
    /// `ExecuteExtraToolResolver::default()`）。
    fn build_tool_resolver(&self) -> Arc<dyn ToolInvocationResolver>;

    /// 构造 error_suggest registry + tool registry snapshot（迁移前语义 =
    /// `build_default_registry()` + `build_tool_registry_snapshot()`）。
    fn build_error_suggest(
        &self,
        cwd: &str,
        tool_names: &[String],
    ) -> (Arc<ErrorSuggestRegistry>, ToolRegistrySnapshot);

    /// 构造 session 级 workflow 中间件实例（`WorkflowMiddleware` upcast 为端口；
    /// 创建点仍在宿主装配面，本方法只做实例化）。
    fn build_workflow_middleware(
        &self,
        executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
        progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
    ) -> Arc<dyn WorkflowMiddlewarePort>;
}

/// Workflow agent 模型构造产物：模型实例 + 有效模型名（GitAttribution 装配用，
/// 语义同迁移前 `effective_provider.model_name()`）。
pub struct WorkflowModel {
    pub model: Arc<dyn peri_model::Model>,
    pub model_name: String,
    /// 请求的模型档位（alias，如 sonnet/haiku）；请求参数是合法 alias 且
    /// 解析成功时有值，否则 None。TUI 面板显示档位而非解析后的真实模型名。
    pub tier: Option<String>,
}

/// Workflow agent 模型工厂（ACP 宿主构造：alias 解析 + `maxTokens` 覆盖 + retry
/// observer 烘焙 + AgentPool 缓存；`peri-agent` 侧不持有 provider 实现）。
///
/// 参数依次为有效模型选择（None = provider 默认）、本次调用的输出 token 上限
/// （None = profile/provider 默认）和 retry observer。每次调用返回新实例——compact
/// 与 base 各持一份，与迁移前 `create_executor` 行为一致。
pub type WorkflowModelFactory = Arc<
    dyn Fn(Option<&str>, Option<u32>, Arc<dyn peri_model::RetryObserver>) -> WorkflowModel
        + Send
        + Sync,
>;

/// Workflow agent 的 subagent prompt 渲染端口。
///
/// agent type 指定时使用其 `agent.md` overrides 渲染；无 agent type 时继续使用
/// session 冻结的默认 subagent prompt。
pub type WorkflowAgentPromptBuilder =
    Arc<dyn Fn(Option<&AgentOverrides>, &str, Option<&str>, Option<&str>) -> String + Send + Sync>;

/// Workflow agent system prompt fallback 渲染闭包（ACP 宿主构造：`PromptTemplate`
/// 渲染面；参数 = cwd / frozen date / frozen language）。
///
/// 仅 `WorkflowAgentContext.system_prompt = None` 时调用（workflow 链不注册
/// WorkflowTool，fallback 渲染关闭 workflow section——P2-2026-08-02）。
pub type WorkflowSystemPromptFallback =
    Arc<dyn Fn(&str, Option<&str>, Option<&str>) -> String + Send + Sync>;

/// Workflow agent 事件发射钩子（ACP 宿主构造：`Controller::publish_event` 适配；
/// 事件三层化统一出口，与主 executor 同一发射路径）。
pub type WorkflowPublishHook = Arc<
    dyn Fn(&str, &peri_acp_types::runtime::UnstampedEvent, &peri_acp_types::event::ExecutorEvent)
        + Send
        + Sync,
>;
