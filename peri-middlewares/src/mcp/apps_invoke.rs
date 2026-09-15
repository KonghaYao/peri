//! Host-initiated MCP Apps invoke: new `tools/call` + new binding lease.

use std::sync::Arc;

use peri_acp_types::{
    mcp_apps::{McpAppInvokeOutcome, McpAppInvokeRequest, McpAppsErrorKind, McpAppsRelayError},
    tools::{
        EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher, EffectiveToolError,
        EffectiveToolErrorCode,
    },
};
use peri_agent::tools::{BaseTool, ToolContext};
use tokio_util::sync::CancellationToken;

use super::{
    apps::{tool_resource_uri, tool_visibility},
    apps_relay::{relay_error, PoolMcpAppsRelay},
    client::{McpClientHandle, McpClientPool},
    tool_bridge::{effective_mcp_tool_name, McpToolBridge, ToolCallError},
};

/// Pool-backed dispatcher stored on **host-issued** App leases.
///
/// Model-issued leases capture the live Act-stage dispatcher (HITL). After
/// Reopen there is no live turn, so subsequent `peri/mcp/app` calls go through
/// `McpToolBridge` on the deployment pool instead of reconstructing a prompt.
#[derive(Clone)]
struct PoolAppToolDispatcher {
    pool: Arc<McpClientPool>,
}

impl PoolAppToolDispatcher {
    fn new(pool: Arc<McpClientPool>) -> Self {
        Self { pool }
    }

    fn lookup_bridge(&self, effective_name: &str) -> Option<McpToolBridge> {
        for client in self.pool.get_all_clients() {
            if let Some(bridge) = bridge_for_effective(&self.pool, &client, effective_name) {
                return Some(bridge);
            }
        }
        None
    }
}

#[async_trait::async_trait]
impl EffectiveToolDispatcher for PoolAppToolDispatcher {
    async fn dispatch(
        &self,
        call: EffectiveToolCall,
        cancel: CancellationToken,
    ) -> Result<String, EffectiveToolError> {
        let Some(bridge) = self.lookup_bridge(&call.tool_name) else {
            return Err(EffectiveToolError::new(
                EffectiveToolErrorCode::UnknownTool,
                format!("unknown MCP App tool {}", call.tool_name),
            ));
        };
        let messages: [peri_acp_types::messages::BaseMessage; 0] = [];
        let ctx = ToolContext::new(&messages, ".").with_effective_tool_dispatcher(
            Arc::new(self.clone()),
            call.invocation_id,
            cancel,
        );
        bridge.invoke(call.input, ctx).await.map_err(|error| {
            EffectiveToolError::new(EffectiveToolErrorCode::ToolFailed, error.to_string())
        })
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        let mut tools = Vec::new();
        for client in self.pool.get_all_clients() {
            for tool in &client.tools {
                tools.push(EffectiveToolDefinition {
                    name: effective_mcp_tool_name(&client.name, tool.name.as_ref()),
                    description: tool
                        .description
                        .as_ref()
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    parameters: serde_json::to_value(&*tool.input_schema)
                        .unwrap_or_else(|_| serde_json::json!({})),
                });
            }
        }
        tools
    }
}

impl PoolMcpAppsRelay {
    pub(super) async fn invoke_app_inner(
        &self,
        request: &McpAppInvokeRequest,
        cancellation: CancellationToken,
    ) -> Result<McpAppInvokeOutcome, McpAppsRelayError> {
        if request.owner_session_id.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::InvalidSession));
        }
        if request.server_id.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::UnknownServer));
        }
        if request.tool_name.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::ToolNotFound));
        }
        let handle = invoke_handle(&self.pool, &request.server_id)?;
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
        let generation = self.pool.handle_generation(&handle);
        let protocol = handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .map(|info| info.protocol_version.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let dispatcher = Arc::new(PoolAppToolDispatcher::new(Arc::clone(&self.pool)));
        let turn_generation = self
            .pool
            .app_binding_leases
            .current_turn(&request.owner_session_id)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let tool_call_id = uuid::Uuid::now_v7().to_string();
        let bridge = McpToolBridge::new(&request.server_id, tool, Arc::clone(&handle))
            .with_server_generation(generation)
            .with_binding_leases(Arc::clone(&self.pool.app_binding_leases));
        let effective_tool_name = bridge.name().to_string();
        let arguments = request.arguments.clone();
        let messages: [peri_acp_types::messages::BaseMessage; 0] = [];
        let ctx = ToolContext::new(&messages, ".")
            .with_effective_tool_dispatcher(
                dispatcher,
                tool_call_id.clone(),
                cancellation.child_token(),
            )
            .with_session_identity(request.owner_session_id.clone(), turn_generation);
        let output = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(relay_error(McpAppsErrorKind::Cancelled));
            }
            result = bridge.invoke(serde_json::Value::Object(arguments.clone()), ctx) => {
                result.map_err(map_tool_error)?
            }
        };
        Ok(McpAppInvokeOutcome {
            mcp_protocol_version: protocol,
            tool_call_id,
            effective_tool_name,
            arguments,
            output,
        })
    }
}

fn invoke_handle(
    pool: &McpClientPool,
    server_id: &str,
) -> Result<Arc<McpClientHandle>, McpAppsRelayError> {
    pool.get_client(server_id)
        .ok_or_else(|| relay_error(McpAppsErrorKind::UnknownServer))
}

fn bridge_for_effective(
    pool: &McpClientPool,
    client: &Arc<McpClientHandle>,
    effective_name: &str,
) -> Option<McpToolBridge> {
    let generation = pool.handle_generation(client);
    for tool in &client.tools {
        let bridge = McpToolBridge::new(&client.name, tool, Arc::clone(client))
            .with_server_generation(generation)
            .with_binding_leases(Arc::clone(&pool.app_binding_leases));
        if bridge.name() == effective_name {
            return Some(bridge);
        }
    }
    None
}

fn map_tool_error(error: Box<dyn std::error::Error + Send + Sync>) -> McpAppsRelayError {
    if let Some(tool_error) = error.downcast_ref::<ToolCallError>() {
        return relay_error(match tool_error {
            ToolCallError::NotConnected { .. } | ToolCallError::Unavailable { .. } => {
                McpAppsErrorKind::ServerDisconnected
            }
            ToolCallError::Timeout { .. } | ToolCallError::CallFailed { .. } => {
                McpAppsErrorKind::UpstreamProtocolError
            }
        });
    }
    relay_error(McpAppsErrorKind::UpstreamProtocolError)
}

#[cfg(test)]
#[path = "apps_invoke_test.rs"]
mod tests;
