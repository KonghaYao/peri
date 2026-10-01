//! Builtin artifact MCP server that exposes the artifact upload tool over rmcp.
//!
//! Tool declaration and IF-D14 result mapping are shared through `peri_mcp_common`;
//! host-side instance dispatch and lifecycle remain in `peri-middlewares`.
//!
//! 边界（sub-plan F §6.2）：
//! - `cwd` 来自实例构造点，只用于相对路径解析，**不是**安全沙箱；`ToolContext` 与
//!   工具自身的 `cwd` 保持一致。
//! - 凭据由实例环境传入，在 Artifact MCP 内解释；url / token 不出现在 `ServerInfo`、
//!   `Tool::description` 或任何错误文本里（本模块从不打印它们）。
//! - 不覆写 `discover`（§10 R2）。

use std::{path::Path, sync::Arc};

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

use crate::ArtifactTool;

/// `artifact` 实例的 `ServerInfo` 名字（`Implementation::name`）。
const ARTIFACT_SERVER_NAME: &str = "peri-artifact-mcp";

/// `artifact` 实例的 handler：持有 `ArtifactTool` 与解析相对路径用的 `cwd`。
pub struct ArtifactMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: String,
}

impl ArtifactMcpServer {
    /// 默认实例构造；未传入专属环境时使用公共服务。
    pub fn new(cwd: &Path) -> Self {
        Self::with_instance_env(cwd, &std::collections::HashMap::new())
    }

    /// MCP 实例环境在构造时冻结，连接重建时由调用方再次提供。
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

    fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl ServerHandler for ArtifactMcpServer {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerInfo {
        server_info(ARTIFACT_SERVER_NAME, env!("CARGO_PKG_VERSION"))
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
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => Ok(CallToolResponse::Complete(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("Tool execution cancelled.")]))),
            result = invoke_tool_call(self.tools(), &self.cwd, &request) => result,
        }
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
