//! LSP 文档同步薄中间件（`ChainSlot::Lsp` 唯一占用者）。
//!
//! A7/A23/A30：LSP **工具面**已迁移到 builtin MCP 实例（
//! `mcp-packages/lsp/src/server.rs` 的 `LspMcpServer`），本中间件不再
//! 实现 `collect_tools`、不构造 `LspTool`、不持有第二份 pool、不启动
//! language server，只把 `Write` / `Edit` 落盘后的文件内容经
//! 注入的 WorkspaceFileReader 从工具执行环境读取，再经
//! [`LspPoolPort`] 同步给路由到的服务器。
//!
//! 会话 cwd 来自 `AfterToolState` 继承的 `StateView::cwd()`：`after_tool` hook
//! 没有 `ToolContext`，相对 `file_path` 只能按该值解析，不假定能拿到工具侧 cwd。
//!
//! 已知限界（A30 / IF-P3-09，按现状登记）：单次 hook 内严格
//! `didChange` → `didSave` 且前者失败仍尝试后者；**跨并发调用不承诺**
//! 全局 FIFO 或 change/save 原子对，本波不新增同步队列 / 版本协议。

use peri_agent::middleware::capabilities as hook_state;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::ports::LspPoolPort;
use peri_agent::{
    agent::react::{ToolCall, ToolResult},
    error::AgentResult,
    middleware::r#trait::Middleware,
};

use crate::tool_search::core_tools::{TOOL_EDIT, TOOL_WRITE};

/// `Write` / `Edit` 落盘后的 LSP 文档同步。
pub struct LspSyncMiddleware {
    port: Arc<dyn LspPoolPort>,
    reader: Arc<dyn crate::workspace_io::WorkspaceFileReader>,
}

impl LspSyncMiddleware {
    pub fn new(
        port: Arc<dyn LspPoolPort>,
        reader: Arc<dyn crate::workspace_io::WorkspaceFileReader>,
    ) -> Self {
        Self { port, reader }
    }
}

/// 绝对路径原样使用；相对路径以会话 cwd（`AfterToolState::cwd()`）解析为绝对路径。
fn resolve_path(cwd: &str, file_path: &str) -> PathBuf {
    let path = Path::new(file_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(cwd).join(path)
    }
}

#[async_trait]
impl Middleware for LspSyncMiddleware {
    fn name(&self) -> &str {
        "LspSyncMiddleware"
    }

    // `collect_tools` 保持默认空集：LSP 工具面只由 `mcp::builtin::lsp` 暴露。
    async fn after_tool(
        &self,
        state: &mut dyn hook_state::AfterToolState,
        tool_call: &ToolCall,
        _result: &ToolResult,
    ) -> AgentResult<()> {
        if tool_call.name != TOOL_WRITE && tool_call.name != TOOL_EDIT {
            return Ok(());
        }

        let Some(file_path) = tool_call.input.get("file_path").and_then(|v| v.as_str()) else {
            tracing::debug!(target: "lsp", tool = %tool_call.name, "LSP 同步跳过：tool_call 无 file_path");
            return Ok(());
        };
        let path = resolve_path(state.cwd(), file_path);

        // `ready_for` 是读文件之前的唯一前置判定：不就绪 ⇒ 不读磁盘、不发任何
        // 通知，也不补拉 language server（A30）。
        if !self.port.ready_for(&path) {
            tracing::debug!(target: "lsp", file = %path.display(), "LSP 同步跳过：无可用且就绪的服务器");
            return Ok(());
        }

        let text = match self.reader.read_text(&path).await {
            Ok(text) => text,
            Err(e) => {
                tracing::debug!(target: "lsp", file = %path.display(), error = %e, "LSP 同步文件时读取失败");
                return Ok(());
            }
        };

        // 内容来自磁盘（不用工具结果文本代替）；两个错误分别 debug 降级，
        // 恒 `Ok(())`，不改写工具结果、不阻断后续工具。
        if let Err(e) = self.port.did_change(&path, &text).await {
            tracing::debug!(target: "lsp", file = %path.display(), error = %e, "LSP didChange 失败");
        }
        if let Err(e) = self.port.did_save(&path).await {
            tracing::debug!(target: "lsp", file = %path.display(), error = %e, "LSP didSave 失败");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
