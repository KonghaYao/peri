//! LSP MCP handler; host runtime and builtin lifecycle remain in `peri-middlewares`.
//!
//! 复用既有 [`LspTool`] ——**不重写** schema、不复制
//! formatter 与 `invoke` 语义，handler 只做 `tools/list` 声明、`tools/call` 路由与
//! IF-D14 结果映射（映射与其他实例共用 [`peri_mcp_common::invoke_tool_call`]，
//! 不另写第二份 name lookup / 参数缺省 / 成功失败映射）。
//!
//! 门控谓词（冻结）：工具面是否可见由**生效 LSP 配置是否非空**决定，唯一判据是
//! `LspServerPool::has_servers()`。该判据只反映生效配置表（`LspServerPool::new` 惰性、
//! 不拉进程），**与 language server 进程是否 ready 无关**：配置里有服务器但一个进程都
//! 没起来时，`LSP` 工具照常可见（可见性是声明面事实，就绪与否由工具自身的
//! `get_initialized_server` 按需处理）。
//!
//! 快照时机：`tools` 在 [`LspMcpServer::new`] 构造时取**一次**；`list_tools` 不重算。
//! 运行期变更配置**不会**改变已构造实例的工具面——配置热更新是**显式非目标**（同一
//! 进程内配置变更的生效路径是重建 handler / 重连，不是本 handler 重读 pool）。
//!
//! 类型边界：`LspServerPool` 经 `peri_resources::lsp::pool` 引入（层级门禁：
//! `peri-middlewares` 不直接依赖 `peri_lsp` crate）。
//!
//! 边界：
//! - **不覆写 `discover`**（§10 R2）：覆写会让同一连接上的 `tools/list` 被会话层以
//!   `-32602` 拒绝（线路级证据见本 crate 的 server tests）；
//! - **不新增** resources / logging / subscriptions 能力（`get_info` 只声明 tools）；
//! - `LspTool::invoke` 不读 `ToolContext`，因此 `tools/call` 传入的 cwd 是空串；
//!   宿主 cwd 透传缺口（F1）归 H-02 / H-05，不在本任务范围。

use std::sync::Arc;

use peri_agent::tools::BaseTool;
use peri_resources::lsp::pool::LspServerPool;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
        ServerInfo,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};

use peri_mcp_common::{invoke_tool_call, list_tools_of, server_info};

use crate::tool::LspTool;

/// `lsp` 实例的 `ServerInfo` 名字（`Implementation::name`）；实例名仍是注册表里的
/// `"lsp"`（`Implementation::name` 与注册表 key 是两件事，不要合并）。
const LSP_SERVER_NAME: &str = "peri-lsp-mcp";

/// `lsp` 实例的 handler：按生效配置快照**一个** `LSP` 工具（无配置时为空表）。
///
/// `pool` 字段**必须保留**（即使 `tools` 为空）：handler 是实例对 pool 的持有者，空配置
/// 实例仍要能握手、`get_info` / 空 `tools/list` 都正常可用；工具集为空**不表示**实例
/// 未就绪（不得用空表反推就绪态）。
pub struct LspMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
    _pool: Arc<LspServerPool>,
}

impl LspMcpServer {
    /// 生产构造：按生效配置快照工具面（`has_servers()` 为真 ⇒ 恰好一个 `LSP` 工具，
    /// 与注册表声明的单工具一致；为假 ⇒ 空表）。
    ///
    /// 快照在此处取一次并冻结：`list_tools` 不重算 `has_servers()`，运行期 `add_server`
    /// 不改变本实例的工具面（热更新是显式非目标）。
    pub fn new(pool: Arc<LspServerPool>) -> Self {
        // `has_servers()` 只看生效配置表（惰性池：此处不启动任何 language server 进程）。
        let tools: Vec<Arc<dyn BaseTool>> = if pool.has_servers() {
            vec![Arc::new(LspTool::new(Arc::clone(&pool)))]
        } else {
            Vec::new()
        };
        Self { tools, _pool: pool }
    }

    /// 测试构造口径的观察点：断言 handler 持有的是**同一个** pool（`Arc::ptr_eq`），
    /// 不是复制或重新装配。
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &Arc<LspServerPool> {
        &self._pool
    }

    fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl ServerHandler for LspMcpServer {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerInfo {
        server_info(LSP_SERVER_NAME, env!("CARGO_PKG_VERSION"))
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
        // `LspTool::invoke` 不读 `ToolContext`（`lsp/tool.rs` 忽略 `_ctx`）。
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
