use super::{
    fmt, Arc, ArtifactTool, BaseTool, CallToolRequestParams, CallToolResponse, Debug, Formatter,
    ListToolsResult, McpError, PaginatedRequestParams, Path, RequestContext, RoleServer,
    ServerConfig, ServerHandler,
};

/// `artifact` 实例的 `ServerConfig` 名字（`Implementation::name`）。
pub(super) const ARTIFACT_SERVER_NAME: &str = "peri-artifact-mcp";

/// `artifact` 实例的 handler：持有 `ArtifactTool` 与解析相对路径用的 `cwd`。
pub struct ArtifactMcpServer {
    pub(super) tools: Vec<Arc<dyn BaseTool>>,
    pub(super) cwd: String,
}

impl Debug for ArtifactMcpServer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactMcpServer")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl ArtifactMcpServer {
    /// 默认实例构造；未传入专属环境时使用公共服务。
    #[must_use]
    pub fn new(cwd: &Path) -> Self {
        Self::with_instance_env(cwd, &std::collections::HashMap::new())
    }

    /// MCP 实例环境在构造时冻结，连接重建时由调用方再次提供。
    #[must_use]
    pub fn with_instance_env(cwd: &Path, env: &std::collections::HashMap<String, String>) -> Self {
        let cwd = cwd.to_string_lossy().into_owned();
        let tool = ArtifactTool::with_instance_env(cwd.clone(), env);
        Self {
            tools: vec![Arc::new(tool)],
            cwd,
        }
    }

    /// 测试构造：注入指向本地桩的 `ArtifactTool`（base url / 假 token 由调用方注入，
    /// 因此成功形态可在无网络条件下走真实上传协议）。
    #[cfg(test)]
    pub(crate) fn with_tools(cwd: &str, tools: Vec<Arc<dyn BaseTool>>) -> Self {
        Self {
            tools,
            cwd: cwd.to_string(),
        }
    }

    pub(super) fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl ServerHandler for ArtifactMcpServer {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerConfig {
        peri_mcp_common::server_info(ARTIFACT_SERVER_NAME, env!("CARGO_PKG_VERSION"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(peri_mcp_common::list_tools_of(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => Ok(CallToolResponse::Complete(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("Tool execution cancelled.")]))),
            result = peri_mcp_common::invoke_tool_call(self.tools(), &self.cwd, &request) => result,
        }
    }
}
