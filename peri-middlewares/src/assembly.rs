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
//! （`McpPoolPort` / `ToolSearchPort` / `CronSchedulerPort`）接入，本模块装配时
//! downcast 还原具体实例。

mod hooks;
mod mcp;
mod preparation;
mod prompt;
// Emscripten：workflow 装配随依赖段（Cargo.toml 同目标 cfg 段）一并移出，不设 wasm 变体。
#[cfg(not(target_os = "emscripten"))]
mod workflow;

// builtin 实例上下文（IF-P3-04 / A33）：宿主装配（`peri-acp`）经这里拿到宿主构造的上下文
// 和 cron 输入；`WorkspaceInstanceInput` 由 `peri-mcp-workspace` 直接公开，宿主直接依赖
// 该能力包，并在 `McpClientPool::run_initialize` 之前经 `with_workspace` 注入。
pub use crate::mcp::builtin::context::{
    BuiltinContextError, BuiltinInstanceContext, CronInstanceInput,
};
#[cfg(not(target_os = "emscripten"))]
pub use peri_mcp_workflow::WorkflowInstanceInput;
#[cfg(not(target_os = "emscripten"))]
pub use workflow::{
    default_workflow_middleware_factory, default_workflow_middleware_factory_with_pool,
    WorkflowAgentMiddlewareFactory,
};

use crate::default_system_prompt::DefaultSystemPromptMiddleware;
use crate::default_system_prompt::LangMiddleware;
use crate::hitl::HumanInTheLoopMiddleware;
use crate::middleware::TodoMiddleware;
use crate::permission::default_requires_approval;
use crate::permission::PermissionMiddleware;
use crate::plugin::PluginMiddleware;
use crate::subagent::SubAgentMiddleware;
use crate::tool_search::ToolSearchMiddleware;
use crate::AtMentionMiddleware;
use crate::GitAttributionMiddleware;
use crate::GoalMiddleware;
use crate::ImageMiddleware;
use parking_lot::RwLock;
use peri_agent::agent::events::AgentEventHandler;
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::chain::MiddlewareChain;
use peri_agent::session::factory::ChainSlot;
use peri_agent::session::factory::MiddlewareChainAssembler;
use peri_agent::session::factory::SubAgentMiddlewarePort;
use peri_agent::tools::BaseTool;
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

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;

#[path = "assembly/production_chain_assembler.rs"]
mod production_chain_assembler;
pub use production_chain_assembler::builtin_closed_instances;
pub(crate) use production_chain_assembler::open_builtin_bridges;
pub use production_chain_assembler::plugin_face_closed;
pub use production_chain_assembler::ProductionChainAssembler;
pub use production_chain_assembler::PLUGIN_FACE_CLOSED_KEY;
pub use production_chain_assembler::SUB_AGENT_FACE_CLOSED_KEY;
