//! `workspace` builtin MCP handler wraps six filesystem tools and `BashTool`; it does not define
//! a second tool schema or execution path. `peri-mcp-common` owns tools/list conversion,
//! tools/call mapping, and safe failure projection.
//!
//! Construction freezes one host cwd and gives it to all seven tools. This does not create a
//! capability root: filesystem tools can still address absolute paths outside cwd, and Bash is
//! not a sandbox. Do not infer stronger isolation from cwd binding.
//!
//! Production uses `standalone`: Workspace owns background Bash tasks and publishes their state
//! over MCP Tasks. The direct `new` constructor retains its injected-manager seam for legacy
//! tool tests; the builtin dispatcher and independent CLI do not use it.
//!
//! The handler does not override `discover`; besides the seven tools it exposes the git ref
//! resource (`workspace://git/ref`) and the 2026-07-28 subscription surface
//! (`accepted_subscription_filter` + `listen`) that carries git ref changes to the host.
//! Tool failures use shared allowlisted recovery text; request cancellation drops the tool
//! future.
//!
//! **W1 resource surface（2026-09-29）**：经 [`Self::with_resources`] 装配时，handler
//! 合并 `resources` provider 的公开批（skills / agents / instructions）与
//! `skills/list|get` 请求（rmcp 3.9.0 起为类型化 `ClientRequest` 变体，经
//! `list_skills` / `get_skill` 提供）；未装配时这些方法分别表现为
//! 「未知资源」（`-32602`）与「方法不支持」（`-32601`）——**不**把缺输入伪装成
//! 「技能不存在且已 ready」。生产装配点（`peri-middlewares` dispatch 的 `workspace`
//! arm）自 W4a（2026-09-29）起对**会话装配**调用 `with_resources`，当前只装载
//! meta 面（J6；skill / agent 根与真实关闭位属 W4b）；顶层三路径不调用（资源面未接线）。
//! 宿主侧投递（registry/消费端切换）除外。

use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::mcp::{McpNotificationMessageKind, MCP_MESSAGE_KIND_META_KEY};
use peri_agent::agent::async_tasks;
use peri_agent::tools::{BaseTool, ToolContext, ToolExecutionStatus};
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CancelTaskParams;
use rmcp::model::CreateTaskResult;
use rmcp::model::CustomRequest;
use rmcp::model::CustomResult;
use rmcp::model::ErrorCode;
use rmcp::model::ExtensionCapabilities;
use rmcp::model::GetMeta;
use rmcp::model::GetSkillRequestMethod;
use rmcp::model::GetSkillRequestParams;
use rmcp::model::GetSkillResult;
use rmcp::model::GetTaskParams;
use rmcp::model::GetTaskResult;
use rmcp::model::Implementation;
use rmcp::model::JsonObject;
use rmcp::model::ListResourceTemplatesResult;
use rmcp::model::ListResourcesResult;
use rmcp::model::ListSkillsRequestMethod;
use rmcp::model::ListSkillsResult;
use rmcp::model::ListToolsResult;
use rmcp::model::MetaObject;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResponse;
use rmcp::model::RequestId;
use rmcp::model::Resource;
use rmcp::model::ResourceContents;
use rmcp::model::ResourceTemplate;
use rmcp::model::ResourceUpdatedNotification;
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::model::ServerNotification;
use rmcp::model::SubscriptionFilter;
use rmcp::model::UpdateTaskParams;
use rmcp::model::TASKS_EXTENSION_ID;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::service::SubscriptionContext;
use rmcp::service::SubscriptionSink;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;

use crate::filesystem::{
    EditFileTool, FolderOperationsTool, GlobFilesTool, GrepTool, ReadFileTool, WriteFileTool,
};
use crate::git_watch;
use crate::git_watch::GitWatchState;
use crate::git_watch::SampleOutcome;
use crate::git_watch::GIT_REF_RESOURCE_URI;
use crate::resources::{
    ResourceBody, ResourceError, WorkspaceResourceProvider, WorkspaceResourcesInput,
};
use crate::shell_tasks::{ShellTaskListTool, ShellTasks, SHELL_TASK_LIST_TOOL};
use crate::terminal::BashTool;
use crate::WorkspaceInstanceInput;
use peri_mcp_common::task_owner as task_owner_functions;

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "workspace_tasks_wire_test.rs"]
mod tasks_wire_tests;

#[path = "workspace/workspace_mcp_server.rs"]
mod workspace_mcp_server;
pub use workspace_mcp_server::WorkspaceMcpServer;
