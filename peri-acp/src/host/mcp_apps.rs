use std::sync::Arc;

use peri_acp_types::mcp_apps::{
    AppSessionBinding, McpAppOpenRequest, McpAppOpenResponse, McpAppRequest, McpAppResponse,
    McpAppsErrorKind, McpAppsRelayError, McpAppsRelayPort, McpResourceRequest, McpResourceResponse,
    MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION,
};
use serde_json::Value;

use super::connection::ConnectionContext;
use crate::transport::types::AcpError;

pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    connection: &mut ConnectionContext,
    relay: Option<&Arc<dyn McpAppsRelayPort>>,
) -> Result<Value, AcpError> {
    if !connection.apps_enabled() {
        return Err(outer_error(McpAppsErrorKind::CapabilityDisabled));
    }
    let relay = relay.ok_or_else(|| outer_error(McpAppsErrorKind::CapabilityDisabled))?;
    match method {
        "peri/mcp/open" => handle_open(params, connection, relay).await,
        "peri/mcp/resource" => handle_resource(params, connection, relay).await,
        "peri/mcp/app" => handle_app(params, connection, relay).await,
        _ => Err(outer_error(McpAppsErrorKind::UnsupportedMethod)),
    }
}

async fn handle_open(
    params: &Value,
    connection: &mut ConnectionContext,
    relay: &Arc<dyn McpAppsRelayPort>,
) -> Result<Value, AcpError> {
    let request: McpAppOpenRequest = decode(params)?;
    validate_versions(&request.envelope_version, &request.apps_protocol_version)?;
    let (mcp_protocol_version, binding) = relay
        .open_app(connection.id(), &request)
        .await
        .map_err(map_relay_error)?;
    if binding.owner_connection_id != connection.id() {
        return Err(outer_error(McpAppsErrorKind::InvalidSession));
    }
    let response = McpAppOpenResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version,
        server_id: binding.server_id.clone(),
        app_session_id: binding.app_session_id.clone(),
        resource_uri: binding.resource_uri.clone(),
    };
    if !connection.insert_app_session(binding) {
        relay.close_connection(connection.id());
        return Err(outer_error(McpAppsErrorKind::InvalidSession));
    }
    serialize(response)
}

async fn handle_resource(
    params: &Value,
    connection: &ConnectionContext,
    relay: &Arc<dyn McpAppsRelayPort>,
) -> Result<Value, AcpError> {
    let request: McpResourceRequest = decode(params)?;
    validate_versions(&request.envelope_version, &request.apps_protocol_version)?;
    let binding = connection
        .app_session(&request.app_session_id)
        .filter(|binding| {
            binding.server_id == request.server_id
                && binding.resource_uri == request.resource_uri
                && binding.apps_protocol_version == request.apps_protocol_version
        })
        .ok_or_else(|| outer_error(McpAppsErrorKind::InvalidSession))?;
    let cancellation = connection.cancellation();
    let mcp_protocol_version = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(outer_error(McpAppsErrorKind::Cancelled));
        }
        result = relay.validate_binding(binding) => result.map_err(map_relay_error)?,
    };
    let (resource_protocol_version, resources) = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(outer_error(McpAppsErrorKind::Cancelled));
        }
        result = relay.read_resource(binding) => result.map_err(map_relay_error)?,
    };
    if resource_protocol_version != mcp_protocol_version {
        return Err(outer_error(McpAppsErrorKind::StaleServerGeneration));
    }
    if !resources
        .iter()
        .any(|resource| resource.is_valid_app_resource(&request.resource_uri))
    {
        return Err(outer_error(McpAppsErrorKind::InvalidResource));
    }
    serialize(McpResourceResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version,
        server_id: request.server_id,
        resources,
    })
}

async fn handle_app(
    params: &Value,
    connection: &ConnectionContext,
    relay: &Arc<dyn McpAppsRelayPort>,
) -> Result<Value, AcpError> {
    let request: McpAppRequest = decode(params)?;
    validate_versions(&request.envelope_version, &request.apps_protocol_version)?;
    if request.payload.method != "tools/call" {
        return Err(outer_error(McpAppsErrorKind::UnsupportedMethod));
    }
    let binding = connection
        .app_session(&request.app_session_id)
        .filter(|binding| {
            binding.server_id == request.server_id
                && binding.resource_uri == request.resource_uri
                && binding.apps_protocol_version == request.apps_protocol_version
        })
        .ok_or_else(|| outer_error(McpAppsErrorKind::InvalidSession))?;
    let cancellation = connection.cancellation();
    let mcp_protocol_version = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(outer_error(McpAppsErrorKind::Cancelled));
        }
        result = relay.validate_binding(binding) => result.map_err(map_relay_error)?,
    };
    let request_id = request.payload.id.clone();
    let (response_protocol_version, payload) = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(outer_error(McpAppsErrorKind::Cancelled));
        }
        result = relay.call_tool(binding, request.payload) => result.map_err(map_relay_error)?,
    };
    if response_protocol_version != mcp_protocol_version {
        return Err(outer_error(McpAppsErrorKind::StaleServerGeneration));
    }
    if payload.id() != &request_id {
        return Err(outer_error(McpAppsErrorKind::UpstreamProtocolError));
    }
    serialize(McpAppResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version,
        server_id: request.server_id,
        app_session_id: request.app_session_id,
        resource_uri: request.resource_uri,
        payload,
    })
}

fn validate_versions(envelope: &str, apps: &str) -> Result<(), AcpError> {
    if envelope != MCP_APPS_ENVELOPE_VERSION {
        return Err(outer_error(McpAppsErrorKind::UnsupportedEnvelopeVersion));
    }
    if apps != MCP_APPS_PROTOCOL_VERSION {
        return Err(outer_error(McpAppsErrorKind::UnsupportedAppsVersion));
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, AcpError> {
    serde_json::from_value(value.clone())
        .map_err(|_| outer_error(McpAppsErrorKind::InvalidEnvelope))
}

fn serialize<T: serde::Serialize>(value: T) -> Result<Value, AcpError> {
    serde_json::to_value(value).map_err(|_| AcpError::new(-32603, "MCP Apps relay request failed"))
}

fn map_relay_error(error: McpAppsRelayError) -> AcpError {
    outer_error(error.kind)
}

fn outer_error(kind: McpAppsErrorKind) -> AcpError {
    let kind =
        serde_json::to_value(kind).unwrap_or(Value::String("upstream_protocol_error".into()));
    AcpError::new(-32000, "MCP Apps relay request failed")
        .with_data(serde_json::json!({"kind": kind}))
}

#[allow(dead_code)]
pub(crate) fn initial_binding(
    owner_connection_id: String,
    owner_session_id: String,
    server_id: String,
    server_generation: u64,
    resource_uri: String,
    instantiating_tool: String,
) -> AppSessionBinding {
    AppSessionBinding {
        app_session_id: uuid::Uuid::now_v7().to_string(),
        owner_connection_id,
        owner_session_id,
        server_id,
        server_generation,
        resource_uri,
        instantiating_tool,
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
    }
}

#[cfg(test)]
#[path = "mcp_apps_test.rs"]
mod tests;
