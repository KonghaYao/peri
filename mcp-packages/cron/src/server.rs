//! Builtin cron MCP server that exposes scheduler tools over rmcp.
//!
//! Tool declaration and IF-D14 result mapping are shared through `peri_mcp_common`;
//! host-side tick supervision and instance lifecycle remain in `peri-middlewares`.
//!
//! 边界（A32：handler **只持工具面**）：
//! - 三个工具各持 `Arc::clone(&scheduler)` 的**同一份** scheduler；handler 内**没有**
//!   任何 tick 驱动（不 spawn task、不调 `CronScheduler::tick`）。tick 归 W2 的 pool
//!   单 spawn 点（`cron::CronSchedulerPort` 装配侧，`assemble.rs` 的 `drive_cron_tick`）：
//!   handler 内再 spawn 一份会与生产 tick 形成双驱动。
//! - 工具级失败经 IF-D14 规则 7 脱敏：`CronError` 原文 / 用户 prompt / 路径都不入
//!   模型面文本（本模块从不拼接这些）。
//! - 不覆写 `discover`（§10 R2）：覆写会让同一连接上的 `tools/list` 被会话层以
//!   `-32602` 拒绝；线路级证据见 `web.rs` 的模块注释与 `cron_test.rs` 的 wire 用例。

use std::sync::Arc;

use parking_lot::Mutex;
use peri_agent::tools::BaseTool;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
        ServerInfo,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};

use peri_mcp_common::{invoke_tool_call, list_tools_of, server_info};

use crate::{CronListTool, CronRegisterTool, CronRemoveTool, CronScheduler};

/// `cron` 实例的 `ServerInfo` 名字（`Implementation::name`）；实例名仍是注册表里的
/// `"cron"`（`Implementation::name` 与注册表 key 是两件事，不要合并）。
const CRON_SERVER_NAME: &str = "peri-cron-mcp";

/// `cron` 实例的 handler：持有三个既有 cron 工具（同一 scheduler，**不含** tick 驱动）。
pub struct CronMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
}

impl CronMcpServer {
    /// 生产构造：工具顺序与注册表声明顺序一致（`cron_register` → `cron_list` →
    /// `cron_remove`），三者各持同一份 scheduler 的 `Arc` 克隆（不复制注册表）。
    pub fn new(scheduler: Arc<Mutex<CronScheduler>>) -> Self {
        let tools: Vec<Arc<dyn BaseTool>> = vec![
            Arc::new(CronRegisterTool::new(Arc::clone(&scheduler))),
            Arc::new(CronListTool::new(Arc::clone(&scheduler))),
            Arc::new(CronRemoveTool::new(Arc::clone(&scheduler))),
        ];
        Self { tools }
    }

    fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl ServerHandler for CronMcpServer {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerInfo {
        server_info(CRON_SERVER_NAME, env!("CARGO_PKG_VERSION"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(list_tools_of(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // cron 三个工具都不读 `ToolContext`（`cron/tools.rs` 的 `invoke` 忽略 `_ctx`）。
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => Ok(CallToolResponse::Complete(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("Tool execution cancelled.")]))),
            result = invoke_tool_call(self.tools(), "", &request) => result,
        }
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
