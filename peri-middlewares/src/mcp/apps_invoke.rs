//! Host-initiated MCP Apps invocation admission.

use peri_acp_types::mcp_apps::{
    McpAppInvokeOutcome, McpAppInvokeRequest, McpAppsErrorKind, McpAppsRelayError,
};
use tokio_util::sync::CancellationToken;

use super::{
    apps::{tool_resource_uri, tool_visibility},
    apps_relay::{relay_error, PoolMcpAppsRelay},
};

impl PoolMcpAppsRelay {
    pub(super) async fn invoke_app_inner(
        &self,
        request: &McpAppInvokeRequest,
        cancellation: CancellationToken,
    ) -> Result<McpAppInvokeOutcome, McpAppsRelayError> {
        if cancellation.is_cancelled() {
            return Err(relay_error(McpAppsErrorKind::Cancelled));
        }
        if request.owner_session_id.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::InvalidSession));
        }
        if request.server_id.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::UnknownServer));
        }
        if request.tool_name.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::ToolNotFound));
        }
        let handle = self
            .pool
            .get_client(&request.server_id)
            .ok_or_else(|| relay_error(McpAppsErrorKind::UnknownServer))?;
        let tool = handle
            .tools
            .iter()
            .find(|tool| tool.name.as_ref() == request.tool_name)
            .ok_or_else(|| relay_error(McpAppsErrorKind::ToolNotFound))?;
        if !tool_visibility(tool).app {
            return Err(relay_error(McpAppsErrorKind::ToolNotAppVisible));
        }
        if tool_resource_uri(tool).is_none() {
            return Err(relay_error(McpAppsErrorKind::InvalidResource));
        }
        if handle.peer.is_none() {
            return Err(relay_error(McpAppsErrorKind::ServerDisconnected));
        }
        tracing::warn!(
            session_id = %request.owner_session_id,
            server_id = %request.server_id,
            tool_name = %request.tool_name,
            "Host-initiated MCP App invocation denied: canonical Permission/HITL dispatcher unavailable"
        );
        Err(relay_error(McpAppsErrorKind::PolicyDenied))
    }
}

#[cfg(test)]
#[path = "apps_invoke_test.rs"]
mod tests;
