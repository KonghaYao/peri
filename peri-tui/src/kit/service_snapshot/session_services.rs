use serde::Deserialize;
use serde_json::json;

use crate::{
    acp_client::AcpTuiClient,
    kit::atoms::{HookSummary, McpInitPhase, McpServerSummary, McpStatusSnapshot, PluginSummary},
};

#[derive(Default)]
pub(super) struct SessionServices {
    pub hooks: Vec<HookSummary>,
    pub plugins: Vec<PluginSummary>,
    pub mcp_servers: Vec<McpServerSummary>,
    pub mcp: McpStatusSnapshot,
}

#[derive(Deserialize)]
struct Plugins {
    hooks: Vec<HookSummary>,
    plugins: Vec<PluginSummary>,
}

#[derive(Deserialize)]
struct McpServers {
    servers: Vec<McpServer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpServer {
    name: String,
    transport: String,
    connection_status: String,
    oauth_status: String,
    tools_count: usize,
}

pub(super) async fn query(client: &AcpTuiClient, session_id: &str) -> SessionServices {
    let params = json!({"sessionId":session_id});
    let (plugins, mcp) = tokio::join!(
        client.send_raw_request("plugin/list", params.clone()),
        client.send_raw_request("mcp/list", params),
    );
    let mut result = SessionServices::default();
    match plugins.and_then(|value| {
        serde_json::from_value::<Plugins>(value)
            .map_err(|error| peri_acp::transport::types::AcpError::new(-32603, error.to_string()))
    }) {
        Ok(plugins) => {
            result.plugins = plugins.plugins;
            result.hooks = plugins
                .hooks
                .into_iter()
                .map(|mut hook| {
                    hook.event.make_ascii_lowercase();
                    hook
                })
                .collect();
        }
        Err(error) => tracing::warn!(%error, "session plugin projection unavailable"),
    }
    match mcp.and_then(|value| {
        serde_json::from_value::<McpServers>(value)
            .map_err(|error| peri_acp::transport::types::AcpError::new(-32603, error.to_string()))
    }) {
        Ok(servers) => {
            result.mcp = McpStatusSnapshot {
                total: servers.servers.len(),
                connected: servers
                    .servers
                    .iter()
                    .filter(|server| server.connection_status == "connected")
                    .count(),
                init_phase: McpInitPhase::Ready,
            };
            result.mcp_servers = servers
                .servers
                .into_iter()
                .map(|server| McpServerSummary {
                    name: server.name,
                    transport: server.transport,
                    status: server.connection_status,
                    needs_auth: server.oauth_status == "needs_authorization",
                    tools_count: server.tools_count,
                    ..McpServerSummary::default()
                })
                .collect();
        }
        Err(_) => result.mcp.init_phase = McpInitPhase::Failed,
    }
    result
}
