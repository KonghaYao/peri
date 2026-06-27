use async_trait::async_trait;
use peri_agent::{middleware::r#trait::Middleware, tools::BaseTool};

use super::{web_fetch::WebFetchTool, web_search::WebSearchTool};

/// Web 中间件，提供 WebFetch 和 WebSearch 工具
pub struct WebMiddleware;

impl WebMiddleware {
    pub fn new() -> Self {
        Self
    }

    /// 构建 Web 工具集，供 parent_tools 等外部直接构造使用
    pub fn build_tools() -> Vec<Box<dyn BaseTool>> {
        vec![
            Box::new(WebFetchTool::new()),
            Box::new(WebSearchTool::new()),
        ]
    }
}

impl Default for WebMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for WebMiddleware {
    fn name(&self) -> &str {
        "WebMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        Self::build_tools()
    }
}

#[cfg(test)]
#[path = "web_test.rs"]
mod tests;
