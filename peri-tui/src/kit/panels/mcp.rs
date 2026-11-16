//! ratatui-kit McpPanel component.
//!
//! H1d（Iteration 14）：从 MCP_SERVERS atom 读取真实 MCP server 列表（由
//! service_snapshot 经 ACP `mcp/list` 查询派生）。结合 SERVICE_SNAPSHOT.mcp
//! 显示初始化阶段摘要。普通 MCP 的重启/关闭经 ACP 会话控制面。
//!
//! OAuth 授权入口（详情视图）：列表选中「需要认证」server 按 Enter 进入
//! 详情视图，详情里选 \[ 授权 \] 按钮（Enter/鼠标点击）才触发 mcp/oauth_start
//! RPC → host 异步授权 → 弹出 OAuthPopup；\[ 返回 \] 或 Esc 回列表。
//!
//! 错误摘要存在时详情 header 提供 \[ 复制 \] 按钮：右对齐渲染在标题（server 名/
//! 状态）行右侧，激活后把错误原文写入剪贴板并留在详情视图；共用 ←→ 选择 +
//! Enter 确认 / 鼠标左键点击的按钮机制。窄终端下标题按预算截断，按钮优先保留。

mod actions;
mod detail;

#[cfg(test)]
#[path = "mcp/detail_tables_test.rs"]
mod detail_tables_test;

#[cfg(test)]
#[path = "mcp/scroll_test.rs"]
mod scroll_test;
#[cfg(test)]
mod tests;

mod mcp_panel;
pub use mcp_panel::*;
