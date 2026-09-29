//! `workspace` builtin MCP handler wraps six filesystem tools and `BashTool`; it does not define
//! a second tool schema or execution path. `peri-mcp-common` owns tools/list conversion,
//! tools/call mapping, and safe failure projection.
//!
//! Construction freezes one host cwd and gives it to all seven tools. This does not create a
//! capability root: filesystem tools can still address absolute paths outside cwd, and Bash is
//! not a sandbox. Do not infer stronger isolation from cwd binding.
//!
//! `WorkspaceInstanceInput` is session-scoped and passes the session task manager and background
//! completion callback to Bash unchanged. `None` is supported: all seven tools remain visible,
//! while Bash rejects explicit background execution, kills a timed-out foreground process group,
//! and cannot register shell descendants left behind by `command &`.
//!
//! The handler does not override `discover` or add resources, logging, or subscriptions. Tool
//! failures use shared allowlisted recovery text; request cancellation drops the tool future.

use std::sync::Arc;

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

use crate::filesystem::{
    EditFileTool, FolderOperationsTool, GlobFilesTool, GrepTool, ReadFileTool, WriteFileTool,
};
use crate::terminal::BashTool;
use crate::WorkspaceInstanceInput;

/// `workspace` 实例的 `ServerInfo` 名字（`Implementation::name`）；实例名仍是注册表里的
/// `"workspace"`（`Implementation::name` 与注册表 key 是两件事，不要合并）。
const WORKSPACE_SERVER_NAME: &str = "peri-workspace-mcp";

/// `workspace` 实例的 handler：持有 7 个既有工具（**同一份** `BaseTool` 实现）与一个
/// 构造期冻结的 host cwd。
///
/// cwd 在构造时取一次；`list_tools` 不重算、运行期不可变更（与 pool 的 `execution_cwd`
/// 同为 `OnceLock` 语义，AW3-05：不支持多 cwd）。
pub struct WorkspaceMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
    /// 工具面共享的 host cwd（文件工具相对路径解析 + Bash `current_dir`）。
    cwd: String,
}

impl WorkspaceMcpServer {
    /// 生产构造：按 cwd 实例化 7 个既有工具，并把 session 级输入（若有）转交 `BashTool`。
    ///
    /// 顺序 = 注册表 `WORKSPACE_TOOLS` 的声明顺序（`list_tools_of` 保留本向量顺序）：
    /// 6 个文件工具 Read → Write → Edit → Glob → Grep → folder_operations，`Bash` 作为
    /// 唯一执行类工具排在末位（与 `namespace()` 的 `filesystem` / `execution` 分组一致）。
    /// v4-part-4 W3-C1 后链上已无 middleware 提供面（原 `FilesystemMiddleware::build_tools`
    /// 的顺序参照随之失效）：本段顺序的**唯一**事实源是注册表声明顺序，两处由
    /// `workspace_test.rs` 的声明段用例锁定。
    ///
    /// `input` 的两名成员各自独立生效（`task_manager` 与 `on_bg_complete` 由不同装配面产出，
    /// 本构造不假定它们同时到位）：`None` 时对应字段保持 `BashTool::new` 的缺省 `None`
    /// （退化分支见模块头），其余 6 个工具不受 `input` 影响。
    pub fn new(cwd: impl Into<String>, input: Option<WorkspaceInstanceInput>) -> Self {
        let cwd = cwd.into();
        let mut bash = BashTool::new(cwd.as_str());
        if let Some(input) = input {
            if let Some(task_manager) = input.task_manager {
                bash = bash.with_task_manager(task_manager);
            }
            if let Some(on_bg_complete) = input.on_bg_complete {
                bash = bash.with_on_bg_complete(on_bg_complete);
            }
        }
        let tools: Vec<Arc<dyn BaseTool>> = vec![
            Arc::new(ReadFileTool::new(cwd.as_str())),
            Arc::new(WriteFileTool::new(cwd.as_str())),
            Arc::new(EditFileTool::new(cwd.as_str())),
            Arc::new(GlobFilesTool::new(cwd.as_str())),
            Arc::new(GrepTool::new(cwd.as_str())),
            Arc::new(FolderOperationsTool::new(cwd.as_str())),
            Arc::new(bash),
        ];
        Self { tools, cwd }
    }

    fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl ServerHandler for WorkspaceMcpServer {
    // 不覆写 `discover`（§10 R2：覆写会让 `tools/list` 被会话层拒绝）。

    fn get_info(&self) -> ServerInfo {
        server_info(WORKSPACE_SERVER_NAME, env!("CARGO_PKG_VERSION"))
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
        // 传实例冻结的 host cwd（不走 `web` / `cron` / `lsp` 的空串形态）。**注意**：本参
        // 只落进 `ToolContext`，而本波 7 个工具都忽略 `ToolContext`（`invoke` 的 `_ctx`），
        // 真正生效的 cwd 绑定点是 [`Self::new`] 注入各工具的 `cwd` 字段——不得据本行推断
        // 「cwd 由 `tools/call` 决定」（反例实验证据见 `workspace_test.rs` 模块头）。
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => Ok(CallToolResponse::Complete(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("Tool execution cancelled.")]))),
            result = invoke_tool_call(self.tools(), &self.cwd, &request) => result,
        }
    }
}
