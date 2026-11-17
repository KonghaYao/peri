//! Builtin cron MCP server that exposes scheduler tools over rmcp.
//!
//! Tool declaration and IF-D14 result mapping are shared through `peri_mcp_common`;
//! host-side tick supervision and instance lifecycle remain in `peri-middlewares`.
//!
//! 边界（A32：handler **只持工具面**）：
//! - 工具各持 `Arc::clone(&scheduler)` 的**同一份** scheduler；handler 内**没有**
//!   任何 tick 驱动（不 spawn task、不调 `CronScheduler::tick`）。tick 归 W2 的 pool
//!   单 spawn 点（`cron::CronSchedulerPort` 装配侧，`assemble.rs` 的 `drive_cron_tick`）：
//!   handler 内再 spawn 一份会与生产 tick 形成双驱动。
//! - 工具级失败经 IF-D14 规则 7 脱敏：`CronError` 原文 / 用户 prompt / 路径都不入
//!   模型面文本（本模块从不拼接这些）。
//! - 不覆写 `discover`（§10 R2）：覆写会让同一连接上的 `tools/list` 被会话层以
//!   `-32602` 拒绝；线路级证据见 `web.rs` 的模块注释与 `cron_test.rs` 的 wire 用例。

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use parking_lot::Mutex;
use peri_agent::tools::BaseTool;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::Implementation;
use rmcp::model::ListResourcesResult;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResponse;
use rmcp::model::ReadResourceResult;
use rmcp::model::Resource;
use rmcp::model::ResourceContents;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;

use crate::{CronListTool, CronRegisterTool, CronRemoveTool, CronScheduler, CronToggleTool};

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;

#[path = "server/cron_mcp_server.rs"]
mod cron_mcp_server;
pub use cron_mcp_server::CronMcpServer;
pub use cron_mcp_server::CRON_JOBS_URI;
