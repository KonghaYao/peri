use std::{collections::HashMap, sync::Arc};

use serde_json::Value;

use super::super::{AcpServerConfig, SessionState};
use crate::transport::{types::AcpError, AcpTransport};

pub(in crate::host) fn handles(method: &str) -> bool {
    matches!(
        method,
        "session/input/enqueue"
            | "session/input/dispatch"
            | "session/input/takeback"
            | "session/input/snapshot"
    )
}

pub(in crate::host) fn prepare(
    method: &str,
    params: &Value,
    sessions: &HashMap<String, SessionState>,
) -> Result<Option<Arc<super::super::workspace::SessionEnvironment>>, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(Value::as_str);
    let state = session_id.and_then(|session_id| sessions.get(session_id));
    if method.starts_with("session/input/") {
        let state = state.ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        if method != "session/input/snapshot" && state.closing {
            return Err(AcpError::new(-32010, "Session is closing"));
        }
    }
    Ok(state.and_then(|state| state.environment.clone()))
}

pub(in crate::host) async fn handle(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn AcpTransport>,
) -> Result<Value, AcpError> {
    match method {
        "session/input/enqueue"
        | "session/input/dispatch"
        | "session/input/takeback"
        | "session/input/snapshot" => {
            super::user_input::handle_prepared_user_input(method, params, cfg, transport).await
        }
        _ => Err(AcpError::new(-32601, "unknown session IO method")),
    }
}
