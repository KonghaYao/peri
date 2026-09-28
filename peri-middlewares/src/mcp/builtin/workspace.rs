//! `workspace` builtin MCP 实例（真实 `ServerHandler`，owner A2 / W3-A）。
//!
//! 复用 peri 现有 7 个工具实现——6 个文件工具（`ReadFileTool` / `WriteFileTool` /
//! `EditFileTool` / `GlobFilesTool` / `GrepTool` / `FolderOperationsTool`，`crate::tools`）
//! 与 `BashTool`（`crate::middleware::terminal`）——**不重写** schema、不复制 `invoke`
//! 语义（AW3-02：本实例是**包装**现有实现，不是在 peri 内再造第二份实现）。handler 只做
//! `tools/list` 声明、`tools/call` 路由与 IF-D14 结果映射（映射与 `web` / `artifact` /
//! `cron` / `lsp` 共用 [`super::web::invoke_tool_call`]，不另写第二份 name lookup /
//! 参数缺省 / 成功失败映射）。
//!
//! cwd 归属（AW3-05 / AW3-04）：实例在构造时取**一个** host cwd 并冻结（与 pool 的
//! `execution_cwd` 同源），7 个工具共享同一份 cwd（文件工具的相对路径解析 + Bash 的
//! `current_dir`）。本波**不引入** capability root：文件工具仍可访问 cwd 之外的绝对路径、
//! Bash 亦无沙箱，这是如实登记的已知缺口（AW3-04；验收记录以 `UNVERIFIED` 口径登记，
//! 不得据本文件的实现宣称已落地）。
//!
//! 边界（与 `web.rs` / `cron.rs` / `lsp.rs` 同）：
//! - **不覆写 `discover`**（主 plan §10 R2）：覆写会让同一连接上的 `tools/list` 被会话层以
//!   `-32602` 拒绝（线路级证据见 `web.rs` 模块注释与 `cron_test.rs` 的 wire 用例）；
//! - **不新增** resources / logging / subscriptions 能力（`get_info` 只声明 tools）；
//! - 工具级失败经 IF-D14 规则 7 脱敏：原始错误（可能内嵌文件路径 / 命令文本）不直接入模型面；
//!   已分类原因及工具生成的任务 ID、PID、日志路径、草稿 ID 经类型化恢复回执保留。
//!
//! ## `BashTool` 的两条能力源：session 级 seam 已落地（AW3-11）
//!
//! 主 plan §3.4 Q1/Q2 的结论是**不通达**：链上 `BashTool` 的 `TaskManager` 是 per-session
//! 对象（`peri-acp/src/session/mod.rs:115` 由每次调用新建的工厂产出），而 `BuiltinInstanceContext`
//! 是 host 级、注入点上不可见任何 session ⇒ 按原样接线，本实例的 `Bash` 会静默退化为
//! 「超时即杀进程组」。用户裁决「先补 seam 再迁 Bash」，seam 的形状冻结在 AW3-11：
//! per-session 的 `TaskManager` 与 bg 完成回调上提为**会话环境装配的输入**，经
//! [`WorkspaceInstanceInput`]（`task_manager` + `on_bg_complete`）送达本 handler，再原样
//! 转交 `BashTool::with_task_manager` / `BashTool::with_on_bg_complete`（两个 builder 的
//! 入参与本输入的两名成员同型，转交不做包装）。
//!
//! **`None` 仍是合法形态（可见但退化，不是遗漏）**：顶层三路径（TUI / print / stdio）与
//! 1:N 形态（`session_resources = true` 当 server root）不产生 session ⇒ 装配面传 `None`，
//! handler 照常构造、7 个工具照常声明，只有 `Bash` 走下列既有退化分支（`middleware/terminal.rs`）：
//! - `run_in_background: true` 直接报错——`:348` 的
//!   `"run_in_background is not available: no background task manager configured"`；
//! - 前台超时不再提升为后台任务，改为杀进程组——`:640` 的「无 TaskManager」分支
//!   （`TimedOut`，无 task_id；有 manager 时是 `RunningAfterTimeout`）；
//! - 前台完成后 `command &` 遗留的子进程无法登记——`:684` 的 `else` 分支走
//!   `release_unmanaged`，Tasks 面板与完成回调（`on_bg_complete`）同样缺失。
//!
//! 该退化分支**进验收记录**（主 plan §4 W3-D 的 **D3**：AW3-11 的 1:N 形态与 `None`
//! 退化登记），不得据本文件的实现宣称「所有路径都带 manager」。

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

use super::context::WorkspaceInstanceInput;
use super::web::{invoke_tool_call, list_tools_of, server_info};
use crate::middleware::terminal::BashTool;
use crate::tools::{
    EditFileTool, FolderOperationsTool, GlobFilesTool, GrepTool, ReadFileTool, WriteFileTool,
};

/// `workspace` 实例的 `ServerInfo` 名字（`Implementation::name`）；实例名仍是注册表里的
/// `"workspace"`（`Implementation::name` 与注册表 key 是两件事，不要合并）。
const WORKSPACE_SERVER_NAME: &str = "peri-workspace-mcp";

/// `workspace` 实例的 handler：持有 7 个既有工具（**同一份** `BaseTool` 实现）与一个
/// 构造期冻结的 host cwd。
///
/// cwd 在构造时取一次；`list_tools` 不重算、运行期不可变更（与 pool 的 `execution_cwd`
/// 同为 `OnceLock` 语义，AW3-05：不支持多 cwd）。
pub(crate) struct WorkspaceMcpServer {
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
    pub(crate) fn new(cwd: impl Into<String>, input: Option<WorkspaceInstanceInput>) -> Self {
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
        server_info(WORKSPACE_SERVER_NAME)
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

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
