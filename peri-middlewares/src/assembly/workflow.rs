//! Workflow agent 的独立装配端口；保留既有工具集与链序。
//!
//! v4-part-4 W3-C1：**无池侧不再有文件/终端工具**（原 `FilesystemMiddleware` /
//! `TerminalMiddleware` 两段已删）。7 个 workspace 工具的唯一提供面是
//! [`BuiltinWorkflowAgentFactory::builtin_tools`]（`open_builtin_bridges`，走注册表
//! `direct` 声明与 `policy_key` 关闭集）——因此本模块在无池/无 McpMiddleware 时
//! 不提供 `Read` / `Write` / `Edit` / `Glob` / `Grep` / `folder_operations` / `Bash`
//! （与 Web/Artifact 的 A6 面③语义同构，见 [`WorkflowAgentMiddlewareFactory`]）。
use crate::hitl::HumanInTheLoopMiddleware;
use crate::mcp::McpClientPool;
use crate::middleware::TodoMiddleware;
use crate::permission::PermissionMiddleware;
use crate::skills::SkillsMiddleware;
use crate::subagent::SkillPreloadMiddleware;
use crate::AgentsMdMiddleware;
use crate::GitAttributionMiddleware;
use peri_agent::agent::workflow::WorkflowAgentContext;
use peri_agent::agent::workflow::WorkflowAgentDefinition;
use peri_agent::agent::workflow::WorkflowAgentFactory;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ToolInvocationResolver;
use std::sync::Arc;

#[path = "workflow/workflow_agent_middleware_factory.rs"]
mod workflow_agent_middleware_factory;
pub use workflow_agent_middleware_factory::default_workflow_middleware_factory;
pub use workflow_agent_middleware_factory::default_workflow_middleware_factory_with_pool;
pub use workflow_agent_middleware_factory::WorkflowAgentMiddlewareFactory;
