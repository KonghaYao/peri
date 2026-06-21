use rmcp::model::RawTextContent;

use super::*;
use crate::mcp::client::ClientStatus;

fn make_tool(name: &str, description: Option<&str>) -> Tool {
    let json = serde_json::json!({
        "name": name,
        "description": description.unwrap_or(""),
        "inputSchema": {
            "type": "object",
            "properties": { "path": { "type": "string" } }
        }
    });
    serde_json::from_value(json).unwrap()
}

fn make_disconnected_handle(name: &str) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Failed("connection lost".to_string()),
        oauth_status: Default::default(),
        source: None,
        url: None,
        channel_capable: false,
    })
}

#[test]
fn test_new_creates_correct_full_name() {
    let tool = make_tool("read_file", Some("Read a file"));
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    assert_eq!(bridge.name(), "mcp__fs__read_file");
}

#[test]
fn test_new_sanitizes_dots_in_names() {
    let tool = make_tool("web.reader", Some("Fetch URL"));
    let handle = make_disconnected_handle("plugin.ctx");
    let bridge = McpToolBridge::new("plugin.ctx", &tool, handle);
    // full_name 净化了非法字符
    assert_eq!(bridge.name(), "mcp__plugin_ctx__web_reader");
    // 但内部 tool_name 保留原始值用于 MCP 协议调用
    assert_eq!(bridge.tool_name, "web.reader");
    assert_eq!(bridge.server_name, "plugin.ctx");
}

#[test]
fn test_new_sanitizes_colons_in_names() {
    let tool = make_tool("query-docs", Some("Query docs"));
    let handle = make_disconnected_handle("context7");
    let bridge = McpToolBridge::new("plugin:context7:context7", &tool, handle);
    assert_eq!(bridge.name(), "mcp__plugin_context7_context7__query-docs");
    assert_eq!(bridge.tool_name, "query-docs");
}

#[test]
fn test_new_creates_correct_description() {
    let tool = make_tool("read_file", Some("Read a file"));
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    assert_eq!(bridge.description(), "[MCP:fs] Read a file");
}

#[test]
fn test_new_preserves_input_schema() {
    let tool = make_tool("read_file", None);
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    let params = bridge.parameters();
    assert!(params.get("properties").is_some());
}

#[test]
fn test_new_empty_description() {
    let tool = make_tool("read_file", None);
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    assert_eq!(bridge.description(), "[MCP:fs] ");
}

#[tokio::test]
async fn test_invoke_not_connected() {
    let tool = make_tool("read_file", None);
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    let result = bridge
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("未连接"));
}

#[test]
fn test_format_content_text_only() {
    let contents = vec![Content {
        raw: rmcp::model::RawContent::Text(RawTextContent {
            text: "hello".to_string(),
            meta: None,
        }),
        annotations: None,
    }];
    assert_eq!(format_contents(&contents), "hello");
}

#[test]
fn test_format_content_mixed() {
    let contents = vec![
        Content {
            raw: rmcp::model::RawContent::Text(RawTextContent {
                text: "line1".to_string(),
                meta: None,
            }),
            annotations: None,
        },
        Content {
            raw: rmcp::model::RawContent::Text(RawTextContent {
                text: "line2".to_string(),
                meta: None,
            }),
            annotations: None,
        },
    ];
    assert_eq!(format_contents(&contents), "line1\nline2");
}

#[test]
fn test_build_tool_bridges_empty_pool() {
    let pool = McpClientPool::new_empty();
    let bridges = build_tool_bridges(&pool);
    assert!(bridges.is_empty());
}
