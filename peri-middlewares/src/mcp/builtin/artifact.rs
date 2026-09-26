//! `artifact` builtin MCP 实例（真实 `ServerHandler`，owner I-01）。
//!
//! 复用 `crate::artifact::ArtifactTool`（md→html 转换、扩展名 / 大小校验、上传协议、
//! 纯文本 URL 输出）——**不重写**上传语义，handler 只做 `tools/list` 声明、
//! `tools/call` 路由与 IF-D14 结果映射（映射与 `web` 实例共用
//! [`super::web::invoke_tool_call`]，不另写第二份）。
//!
//! 边界（sub-plan F §6.2）：
//! - `cwd` 来自实例构造点，只用于相对路径解析，**不是**安全沙箱；`ToolContext` 与
//!   工具自身的 `cwd` 保持一致。
//! - 凭据（`PERI_ARTIFACTS_URL` / `PERI_ARTIFACTS_TOKEN`）在 `ArtifactTool::new`
//!   内读取 ⇒ 读取点 = 实例构造点；url / token 不出现在 `ServerInfo`、
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

use super::web::{invoke_tool_call, list_tools_of, server_info};
use crate::artifact::ArtifactTool;

/// `artifact` 实例的 `ServerInfo` 名字（`Implementation::name`）。
const ARTIFACT_SERVER_NAME: &str = "peri-artifact-mcp";

/// `artifact` 实例的 handler：持有 `ArtifactTool` 与解析相对路径用的 `cwd`。
pub(crate) struct ArtifactMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: String,
}

impl ArtifactMcpServer {
    /// 生产构造：`ArtifactTool::new(cwd)`（客户端在此按 env 构造，凭据绑定到该实例）。
    pub(crate) fn new(cwd: &Path) -> Self {
        let cwd = cwd.to_string_lossy().into_owned();
        let tool = ArtifactTool::new(cwd.clone());
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
        server_info(ARTIFACT_SERVER_NAME)
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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        invoke_tool_call(self.tools(), &self.cwd, &request).await
    }
}

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;
