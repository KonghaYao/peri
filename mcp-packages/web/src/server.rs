//! The builtin web MCP server, exposing the web fetch and search tools over rmcp.
//!
//! Shared tool declaration and IF-D14 result mapping live in `peri_mcp_common`;
//! host-side instance dispatch and lifecycle remain in `peri-middlewares`.

use std::sync::Arc;

use crate::{web_fetch::WebFetchTool, web_search::WebSearchTool};
use peri_agent::tools::BaseTool;
use peri_mcp_common::{invoke_tool_call, list_tools_of, server_info};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
        ServerConfig,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};

/// `web` 实例的 `ServerConfig` 名字（`Implementation::name`）。
const WEB_SERVER_NAME: &str = "peri-web-mcp";

/// `web` 实例的 handler：持有 `WebSearch` / `WebFetch` 两个既有工具。
///
/// Web 工具无状态、无凭据、无 capability root（`cwd` 不被读取），因此 handler 是纯函数式的。
pub struct WebMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
}

impl WebMcpServer {
    /// 生产构造：工具顺序与注册表声明顺序一致（`WebSearch` → `WebFetch`）。
    pub fn new() -> Self {
        let tools: Vec<Arc<dyn BaseTool>> = vec![
            Arc::new(WebSearchTool::new()),
            Arc::new(WebFetchTool::new()),
        ];
        Self { tools }
    }

    /// 测试构造：注入替身工具（MCP 链路两侧都真实，只有工具结果是受控的）。
    ///
    /// Web 两个工具的后端地址在生产恒为编译期常量；测试可经工具自身的
    /// `#[cfg(test)] with_endpoint_for_test` 指向本地回环桩（真实 HTTP），该形态见
    /// `web_test.rs::web_handler_tools_call_reaches_real_http_stub_over_wire`。
    /// 本构造器提供的是「测试替身工具」路径；生产构造 [`Self::new`] 的工具集另有断言覆盖。
    #[cfg(test)]
    pub(crate) fn with_tools(tools: Vec<Arc<dyn BaseTool>>) -> Self {
        Self { tools }
    }

    fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl Default for WebMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerHandler for WebMcpServer {
    // 不覆写 `discover`（§10 R2：覆写会让 `tools/list` 被会话层拒绝）。

    fn get_info(&self) -> ServerConfig {
        server_info(WEB_SERVER_NAME, env!("CARGO_PKG_VERSION"))
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
        // Web 工具忽略 `cwd`（`web_search.rs` / `web_fetch.rs` 的 `invoke` 不读 `ToolContext`）。
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
