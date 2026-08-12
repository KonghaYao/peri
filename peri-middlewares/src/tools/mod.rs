pub mod ask_user_tool;
pub mod filesystem;
pub mod output_persist;
pub mod output_truncate;
pub mod todo;

use std::sync::Arc;

pub use ask_user_tool::AskUserTool;
use async_trait::async_trait;
pub use filesystem::{
    EditFileTool, FolderOperationsTool, GlobFilesTool, GrepTool, ReadFileTool, WriteFileTool,
};
use peri_agent::tools::BaseTool;
pub use todo::{TodoItem, TodoStatus, TodoWriteTool};

/// 严格解析 JSON 数值参数（工具共享，禁止静默回退）。
///
/// - 缺省（null）：`Ok(None)`，由调用方取文档化的默认值；
/// - 非负整数（含 0）：`Ok(Some(n))`；
/// - 浮点（如 `12.5`、`139.0`）、负数、字符串等：`Err`，不再像 `as_u64()`
///   那样静默吞掉并回退默认值——静默回退会让模型以为参数已生效，
///   实际却读到了错误的位置/数量（Read 工具 offset 事故的根因之一）。
pub(crate) fn parse_optional_u64(
    value: &serde_json::Value,
    name: &str,
) -> Result<Option<u64>, Box<dyn std::error::Error + Send + Sync>> {
    if value.is_null() {
        return Ok(None);
    }
    let n = value
        .as_f64()
        .ok_or_else(|| format!("Error: '{name}' must be a non-negative integer, got {value}"))?;
    if n.fract() != 0.0 || n < 0.0 {
        return Err(format!("Error: '{name}' must be a non-negative integer, got {n}").into());
    }
    Ok(Some(n as u64))
}

/// ArcToolWrapper - 将 Arc<dyn BaseTool> 包装为 Box<dyn BaseTool> 可用的形式
///
/// 用于子 agent 注册父 agent 的工具集时，避免所有权转移：
/// 父工具集存为 Arc<Vec<Arc<dyn BaseTool>>>，子 agent 注册时用 ArcToolWrapper 包一层。
pub struct ArcToolWrapper(pub Arc<dyn BaseTool>);

/// BoxToolWrapper - 将 Box<dyn BaseTool> 包装为 Arc<dyn BaseTool> 可用的形式
///
/// 用于将 Middleware::collect_tools() 返回的 Box<dyn BaseTool> 转换为
/// SubAgentMiddleware 所需的 Arc<dyn BaseTool>，以便共享父工具集。
pub struct BoxToolWrapper(pub Box<dyn BaseTool + Send + Sync>);

#[async_trait]
impl BaseTool for BoxToolWrapper {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn is_direct(&self) -> bool {
        self.0.is_direct()
    }

    fn description(&self) -> &str {
        self.0.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.0.parameters()
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.invoke(input, ctx).await
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        self.0.timeout()
    }

    fn aliases(&self) -> &[&str] {
        self.0.aliases()
    }
}

#[async_trait]
impl BaseTool for ArcToolWrapper {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn is_direct(&self) -> bool {
        self.0.is_direct()
    }

    fn description(&self) -> &str {
        self.0.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.0.parameters()
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.invoke(input, ctx).await
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        self.0.timeout()
    }

    fn aliases(&self) -> &[&str] {
        self.0.aliases()
    }
}
