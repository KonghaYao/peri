//! The builtin web MCP server, exposing the web fetch and search tools over rmcp.
//!
//! Shared tool declaration and IF-D14 result mapping live in `peri_mcp_common`;
//! host-side instance dispatch and lifecycle remain in `peri-middlewares`.
//!
//! 资源面（`resources/list|read`）发布 `web-researcher` 定义：
//! `agent://builtin/web-researcher/agent.md`。宿主 Agent registry 只信任宿主绑定
//! 的 builtin 实例来源，因此定义随实例可用性一起开闭。

use std::sync::Arc;

use crate::{
    agents, agents::AgentResourceError, web_fetch::WebFetchTool, web_search::WebSearchTool,
};
use peri_agent::tools::BaseTool;
use peri_mcp_common::{invoke_tool_call, list_tools_of};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, Implementation, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ServerCapabilities, ServerConfig,
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
    /// 默认实例构造：工具顺序与注册表声明顺序一致（`WebSearch` → `WebFetch`）。
    pub fn new() -> Self {
        Self::with_instance_env(&std::collections::HashMap::new())
    }

    /// MCP 实例环境构造；未提供的变量回退到编译期默认后端。
    ///
    /// 实例环境在连接建立时冻结，连接重建时由调用方再次提供（与 `artifact` 实例
    /// 同一约定；凭据值不出现在 `ServerConfig`、工具描述或错误文本里）。
    pub fn with_instance_env(env: &std::collections::HashMap<String, String>) -> Self {
        let tools: Vec<Arc<dyn BaseTool>> = vec![
            Arc::new(WebSearchTool::from_instance_env(env)),
            Arc::new(WebFetchTool::from_instance_env(env)),
        ];
        Self { tools }
    }

    /// 测试构造：注入替身工具（MCP 链路两侧都真实，只有工具结果是受控的）。
    ///
    /// 真实工具的后端地址经实例 env（`PERI_WEB_SEARCH_URL` / `PERI_WEB_FETCH_URL`）
    /// 指向本地回环桩（真实 HTTP），该形态见
    /// `web_test.rs::web_handler_tools_call_reaches_real_http_stub_over_wire`。
    /// 本构造器提供的是「测试替身工具」路径；生产构造 [`Self::new`] /
    /// [`Self::with_instance_env`] 的工具集另有断言覆盖。
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

    /// 能力位：tools（两个 Web 工具）+ resources（`web-researcher` 定义）。
    ///
    /// **不复用** `peri_mcp_common::server_info`：它的固定形状是 tools-only。
    /// 资源能力随静态定义一起声明（「声明即实现」；未声明时客户端不会调用
    /// `resources/list`，宿主 Agent registry 也发现不到该定义）。
    fn get_info(&self) -> ServerConfig {
        let caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        ServerConfig::new(caps).with_server_info(Implementation::new(
            WEB_SERVER_NAME,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(list_tools_of(self.tools()))
    }

    /// 资源发现批：静态 Agent 定义（无跨请求扫描，解析在请求期完成）。
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(agents::list_resources()))
    }

    /// 读取 `agent://builtin/web-researcher/agent.md`。
    ///
    /// 非法/非 agent URI → `-32602`；形状合法但不在本实例公开表 → `-32002`
    /// （与 workspace 资源面同口径；新协议版本下 SDK 按 SEP-2164 升级为 `-32602`）。
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let contents = agents::read(&request.uri).map_err(|error| match error {
            AgentResourceError::InvalidUri => {
                McpError::invalid_params(format!("unknown resource: {}", request.uri), None)
            }
            AgentResourceError::NotFound => {
                McpError::resource_not_found("resource not found", None)
            }
        })?;
        Ok(ReadResourceResponse::Complete(
            rmcp::model::ReadResourceResult::new(vec![contents]),
        ))
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
