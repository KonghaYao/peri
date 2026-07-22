//! Tests for mid_mcp

use super::*;

#[test]
fn test_name_returns_mcp_middleware() {
    let pool = Arc::new(McpClientPool::new_empty());
    let mw = McpMiddleware::new(pool);
    let name = <McpMiddleware as Middleware>::name(&mw);
    assert_eq!(name, "McpMiddleware");
}

#[test]
fn test_collect_tools_empty_pool() {
    let pool = Arc::new(McpClientPool::new_empty());
    let mw = McpMiddleware::new(pool);
    let tools = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    assert!(tools.is_empty());
}
