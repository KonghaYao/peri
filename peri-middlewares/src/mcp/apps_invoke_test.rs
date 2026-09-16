use super::*;
use crate::mcp::client::{ClientStatus, McpClientHandle, McpClientPool};
use peri_acp_types::mcp_apps::{
    McpAppInvokeRequest, MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION,
};
use rmcp::model::Tool;
use serde_json::json;

fn invoke_request(server_id: &str, tool_name: &str, session: &str) -> McpAppInvokeRequest {
    McpAppInvokeRequest {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        server_id: server_id.into(),
        tool_name: tool_name.into(),
        owner_session_id: session.into(),
        arguments: serde_json::Map::from_iter([(
            "source".into(),
            json!("export default function App(){return null}"),
        )]),
    }
}

fn ui_tool(name: &str, resource: Option<&str>, visibility: &[&str]) -> Tool {
    let mut ui = serde_json::Map::new();
    ui.insert("visibility".into(), json!(visibility));
    if let Some(resource) = resource {
        ui.insert("resourceUri".into(), json!(resource));
    }
    serde_json::from_value(json!({
        "name": name,
        "description": name,
        "inputSchema": {"type": "object"},
        "_meta": {"ui": ui}
    }))
    .unwrap()
}

fn insert_server(pool: &McpClientPool, name: &str, tools: Vec<Tool>) {
    pool.clients.write().insert(
        name.to_string(),
        Arc::new(McpClientHandle {
            name: name.to_string(),
            version: None,
            cache_version: None,
            peer: None,
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        }),
    );
}

async fn invoke_kind(pool: McpClientPool, request: McpAppInvokeRequest) -> McpAppsErrorKind {
    let relay = PoolMcpAppsRelay::new(Arc::new(pool));
    relay
        .invoke_app_inner(&request, CancellationToken::new())
        .await
        .unwrap_err()
        .kind
}

#[tokio::test]
async fn invoke_unknown_server_fails_closed() {
    let pool = McpClientPool::new_empty();
    assert_eq!(
        invoke_kind(pool, invoke_request("missing", "show_canvas", "session")).await,
        McpAppsErrorKind::UnknownServer
    );
}

#[tokio::test]
async fn invoke_empty_session_fails_closed() {
    let pool = McpClientPool::new_empty();
    assert_eq!(
        invoke_kind(pool, invoke_request("cursor-canvas", "show_canvas", "  ")).await,
        McpAppsErrorKind::InvalidSession
    );
}

#[tokio::test]
async fn invoke_missing_tool_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool("other", Some("ui://app"), &["app"])],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::ToolNotFound
    );
}

#[tokio::test]
async fn invoke_model_only_tool_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool(
            "show_canvas",
            Some("ui://cursor-canvas/mcp-app.html"),
            &["model"],
        )],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::ToolNotAppVisible
    );
}

#[tokio::test]
async fn invoke_tool_without_ui_resource_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool("show_canvas", None, &["app"])],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::InvalidResource
    );
}

#[tokio::test]
async fn invoke_disconnected_app_tool_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool(
            "show_canvas",
            Some("ui://cursor-canvas/mcp-app.html"),
            &["app"],
        )],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::ServerDisconnected
    );
}
