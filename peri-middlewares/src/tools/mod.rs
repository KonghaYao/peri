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
}

#[async_trait]
impl BaseTool for ArcToolWrapper {
    fn name(&self) -> &str {
        self.0.name()
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
}
