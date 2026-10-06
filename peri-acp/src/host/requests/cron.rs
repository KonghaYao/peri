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
    let scheduler = environment
        .cfg
        .cron_scheduler
        .as_ref()
        .ok_or_else(|| AcpError::new(-32601, "session cron capability unsupported"))?;
    if method == "cron/list" {
        return Ok(json!({"jobs": scheduler.list_tasks()}));
    }
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AcpError::new(-32602, "missing cron id"))?;
    let found = match method {
        "cron/toggle" => scheduler.toggle(id),
        "cron/remove" => scheduler.remove(id),
        _ => return Err(AcpError::new(-32601, "unknown cron method")),
    };
    if !found {
        return Err(AcpError::new(-32602, "cron job not found in session"));
    }
    Ok(json!({"id": id, "success": true}))
}
