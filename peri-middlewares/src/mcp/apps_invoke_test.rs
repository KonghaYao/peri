use super::*;
use crate::mcp::client::{ClientStatus, McpClientHandle, McpClientPool};
use peri_acp_types::mcp_apps::{
    McpAppInvokeRequest, McpAppsRelayPort, MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION,
};
use rmcp::model::Tool;
use serde_json::json;
use std::sync::Arc;

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

struct AppTaskOwner {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl rmcp::ServerHandler for AppTaskOwner {
    async fn call_tool(
        &self,
        _: rmcp::model::CallToolRequestParams,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(rmcp::model::CallToolResponse::Task(
            rmcp::model::CreateTaskResult::new(rmcp::model::Task::new(
                "app-task",
                rmcp::model::TaskStatus::Working,
                "2026-10-05T00:00:00Z",
                "2026-10-05T00:00:00Z",
            )),
        ))
    }
}

#[tokio::test]
async fn host_invoke_without_canonical_approval_denies_before_server_call() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = crate::mcp::client::output_store::tests::Wire::connect(AppTaskOwner {
        calls: calls.clone(),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    insert_server(
        &pool,
        "app-owner",
        vec![ui_tool("large", Some("ui://app"), &["app"])],
    );
    {
        let mut clients = pool.clients.write();
        Arc::get_mut(clients.get_mut("app-owner").unwrap())
            .unwrap()
            .peer = Some(wire.client.peer().clone());
    }
    let relay = PoolMcpAppsRelay::new(pool.clone());
    let mut request = invoke_request("app-owner", "large", "child");
    request.arguments.insert("session_id".into(), json!("root"));
    let error = relay
        .invoke_app(&request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(error.kind, McpAppsErrorKind::PolicyDenied);
    assert_eq!(pool.app_binding_leases.current_turn("child"), None);
    pool.clients.write().clear();
    wire.close().await;
}

#[tokio::test]
async fn cancelled_host_invoke_fails_before_admission() {
    let relay = PoolMcpAppsRelay::new(Arc::new(McpClientPool::new_empty()));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = relay
        .invoke_app(&invoke_request("app-owner", "large", "child"), cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.kind, McpAppsErrorKind::Cancelled);
}
