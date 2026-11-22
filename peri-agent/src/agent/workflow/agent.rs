//! Workflow agent 执行体（p1-wa 归位：自 `peri-acp::host::workflow_agent` 迁入）。
//!
//! 当 Node workflow engine 调用 agent(prompt) 时，`WorkflowRunner` 经
//! [`AgentExecutor`](peri_acp_types::workflow::AgentExecutor) trait 回调本模块，通过 `build_v2_subagent_context` +
//! `run_react_loop` 执行并返回结果。
//!
//! 复用 SubAgent v2 基础设施：workflow agent 携带 frozen CLAUDE.md / skills
//! 并经过完整中间件链（Filesystem/Terminal/Web）。
//!
//! # 依赖反转（p1-wa 收口）
//!
//! §0 边 8（Agent 禁入 Middleware/Controller）：执行体所需的 ACP/Controller/
//! Middleware 特有构造全部经注入面参数化（`WorkflowAgentContext` 字段）：
//!
//! - 模型构造（provider alias 解析 / AgentPool 缓存 / retry observer 烘焙）
//!   → [`WorkflowModelFactory`](crate::agent::workflow::factory::WorkflowModelFactory)（ACP 宿主构造）
//! - 中间件链 / 工具 / tool resolver 装配
//!   → [`WorkflowAgentFactory`](crate::agent::workflow::factory::WorkflowAgentFactory)（peri-middlewares 实现，ACP 宿主注入）
//! - system prompt fallback 渲染 → [`WorkflowSystemPromptFallback`](crate::agent::workflow::factory::WorkflowSystemPromptFallback)（ACP 宿主）
//! - EventBus forwarder 启动（v2 → v1 映射 + biased select 不变量单点）
//!   → `ForwarderLauncherFn`（ACP 宿主构造）
//! - 事件发射（`Controller::publish_event` 统一出口）→ [`WorkflowPublishHook`](crate::agent::workflow::factory::WorkflowPublishHook)
//! - Langfuse 观测（turn 钩子 + 事件旁路）→ `LangfuseHooks` / 事件处理闭包
//!
//! 迁移前 `create_session_workflow_middleware`（session 级 WorkflowMiddleware
//! 装配编排）保留在 ACP 装配面（`host/workflow_agent.rs` 薄壳），本模块只
//! 承载执行单元。

mod workflow_agent_executor;

mod execution;
mod observation;
mod report;
mod result;

pub use workflow_agent_executor::WorkflowLangfuseEventHandler;

pub use workflow_agent_executor::WorkflowAgentContext;

pub use workflow_agent_executor::WorkflowAgentExecutor;

pub use workflow_agent_executor::create_executor;

pub use workflow_agent_executor::create_default_executor;

#[cfg(test)]
use workflow_agent_executor::requested_model;

#[cfg(test)]
use workflow_agent_executor::resolve_iteration_limit;

#[cfg(test)]
use workflow_agent_executor::await_workflow_forwarder;

#[cfg(test)]
use workflow_agent_executor::tool_name_in;

#[cfg(test)]
pub(crate) use workflow_agent_executor::workflow_model_bridge;

#[cfg(test)]
use workflow_agent_executor::select_workflow_system_prompt;

#[cfg(test)]
#[path = "agent/agent_test.rs"]
mod tests;

#[cfg(test)]
#[path = "agent/settlement_test.rs"]
mod settlement_tests;
