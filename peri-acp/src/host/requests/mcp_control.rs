//! Session-scoped ACP admission and wire projection for MCP runtime control.

use std::{collections::HashMap, sync::Arc};

use peri_acp_types::mcp_control::{McpControlError, McpControlRequest};
use serde_json::{json, Value};

use crate::{
    host::{connection::ConnectionFacts, workspace::SessionEnvironment, SessionState},
    transport::types::AcpError,
};

pub(crate) fn error(code: i64, kind: &str, message: impl Into<String>) -> AcpError {
    AcpError::new(code, message).with_data(json!({"kind": kind}))
}

pub(crate) fn project_error(failure: McpControlError) -> AcpError {
    tracing::warn!(kind = %failure.kind, error = %failure.message, "MCP control failed");
    error(i64::from(failure.code), &failure.kind, failure.message)
}

pub(crate) fn session_environment(
    params: &Value,
    sessions: &HashMap<String, SessionState>,
) -> Result<Arc<SessionEnvironment>, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| error(-32602, "invalid_request", "missing sessionId"))?;
    let state = sessions
        .get(session_id)
        .ok_or_else(|| error(-32001, "session_not_found", "session not found"))?;
    if state.closing {
        return Err(error(-32000, "session_closed", "session is closing"));
    }
    state.environment.clone().ok_or_else(|| {
        error(
            -32001,
            "session_not_found",
            "session MCP environment is not established",
        )
    })
}

pub(crate) struct PreparedControl {
    environment: Arc<SessionEnvironment>,
    request: McpControlRequest,
}

pub(crate) fn prepare(
    params: &Value,
    connection: &ConnectionFacts,
    sessions: &HashMap<String, SessionState>,
) -> Result<PreparedControl, AcpError> {
    if !connection.mcp_control_v1 {
        return Err(error(
            -32601,
            "capability_not_negotiated",
            "MCP control capability not negotiated",
        ));
    }
    let request: McpControlRequest = serde_json::from_value(params.clone())
        .map_err(|cause| error(-32602, "invalid_request", cause.to_string()))?;
    request.validate().map_err(project_error)?;
    let environment = session_environment(params, sessions)?;
    if !environment.mcp_control_enabled {
        return Err(error(
            -32000,
            "unsupported",
            "MCP runtime control is disabled for this session",
        ));
    }
    if environment.cfg.mcp_pool.is_none() {
        return Err(error(
            -32000,
            "unsupported",
            "MCP runtime control is unavailable",
        ));
    }
    Ok(PreparedControl {
        environment,
        request,
    })
}

pub(crate) async fn execute(prepared: PreparedControl) -> Result<Value, AcpError> {
    let pool = prepared
        .environment
        .cfg
        .mcp_pool
        .clone()
        .ok_or_else(|| error(-32000, "pool_closed", "MCP pool is unavailable"))?;
    let response = pool
        .control_mcp(prepared.request)
        .await
        .map_err(project_error)?;
    serde_json::to_value(response).map_err(|cause| error(-32603, "internal", cause.to_string()))
}
