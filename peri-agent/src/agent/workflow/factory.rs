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

use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult};

use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::middleware::r#trait::Middleware;
use crate::tools::{BaseTool, ToolInvocationResolver};

use super::WorkflowAgentContext;

/// Workflow agent 中间件/工具装配端口。
///
/// `peri-middlewares` 实现（`assembly::workflow_agent::WorkflowAgentMiddlewareFactory`）；
/// 方法面 = workflow agent 执行体所需的全部中间件/工具装配，返回类型一律为
/// Agent 层/契约层类型（实现方经 re-export 构造，不产生 ACP/Middleware 依赖）。
pub trait WorkflowMiddlewareFactory: Send + Sync {
    /// 装配 workflow agent 工具列表（filesystem / terminal / web / skills tools；
    /// 仅 project-level skills，与迁移前行为一致）。
    fn build_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>>;

    /// 装配 workflow agent 中间件链（frozen CLAUDE.md / skills summary / HITL
    /// broker+permission_mode 语义自 `ctx` 读取；`model_name` 供
    /// GitAttribution 使用——alias 解析后的有效模型名）。
    fn build_middlewares(
        &self,
        ctx: &WorkflowAgentContext,
        model_name: &str,
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
}

/// Workflow agent 模型工厂（ACP 宿主构造：alias 解析 + retry observer 烘焙 +
/// AgentPool 缓存；`peri-agent` 侧不持有 provider 实现）。
///
/// 参数 = workflow script 指定的 model（`None` = provider 默认模型）+ 本 run 的
/// retry observer（重试观测翻译为 `LlmRetrying` 交给本 run handler，语义同
/// 主 executor 的 per-turn observer）。每次调用返回新实例——compact 与 base
/// 各持一份，与迁移前 `create_executor` 行为一致。
pub type WorkflowModelFactory =
    Arc<dyn Fn(Option<&str>, Arc<dyn peri_model::RetryObserver>) -> WorkflowModel + Send + Sync>;

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
