use std::collections::HashMap;

use serde_json::{json, Value};

use super::super::SessionState;
use crate::transport::types::AcpError;

pub(super) async fn handle(
    method: &str,
    params: &Value,
    sessions: &HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let session = sessions
        .get(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    if session.closing {
        return Err(AcpError::new(-32602, "session is closing"));
    }
    let environment = session
        .environment
        .as_ref()
        .ok_or_else(|| AcpError::new(-32603, "session environment unavailable"))?;
    let workspace = super::super::workspace::validate_expected(
        &environment.cfg,
        session_id,
        Some(&session.cwd),
    )
    .await?;
    if workspace.cwd != std::path::Path::new(&session.cwd) {
        return Err(AcpError::new(
            -32602,
            "session cron workspace scope mismatch",
        ));
    }
    // This is an admission check only. Business data and mutations must cross
    // the session's MCP connection, including for in-process builtin servers.
    environment
        .cfg
        .cron_scheduler
        .as_ref()
        .ok_or_else(|| AcpError::new(-32601, "session cron capability unsupported"))?;
    let pool = environment
        .cfg
        .mcp_pool
        .as_ref()
        .ok_or_else(|| AcpError::new(-32603, "session MCP pool unavailable"))?;
    if method == "cron/list" {
        let text = pool
            .read_instance_resource("cron", "cron://jobs")
            .await
            .map_err(|error| AcpError::new(-32603, error))?;
        let result: Value = serde_json::from_str(&text).map_err(|error| {
            AcpError::new(-32603, format!("invalid cron jobs resource: {error}"))
        })?;
        if !result.get("jobs").is_some_and(Value::is_array) {
            return Err(AcpError::new(-32603, "cron jobs resource is missing jobs"));
        }
        return Ok(result);
    }
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AcpError::new(-32602, "missing cron id"))?;
    let tool = match method {
        "cron/toggle" => "cron_toggle",
        "cron/remove" => "cron_remove",
        _ => return Err(AcpError::new(-32601, "unknown cron method")),
    };
    pool.call_instance_tool("cron", tool, json!({"id": id}), None)
        .await
        .map_err(|error| AcpError::new(-32603, error))?;
    Ok(json!({"id": id, "success": true}))
}
