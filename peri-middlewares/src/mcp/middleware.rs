use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{middleware::r#trait::Middleware, tools::BaseTool};

use super::{
    client::McpClientPool, resource_tool::McpResourceTool, tool_bridge::build_tool_bridges,
};

/// MCP 中间件 —— 将所有已连接 MCP 服务器的工具和资源注入 ReAct 循环
pub struct McpMiddleware {
    pool: Arc<McpClientPool>,
}

impl McpMiddleware {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Middleware for McpMiddleware {
    fn name(&self) -> &str {
        "McpMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        let mut tools = build_tool_bridges(&self.pool);

        if self.pool.has_resources() {
            tools.push(Box::new(McpResourceTool::new(Arc::clone(&self.pool))));
        }

        tools
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
