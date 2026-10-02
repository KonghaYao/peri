//! Storage v2 metadata and user archive actions.

use serde_json::{json, Value};

use super::super::AcpServerConfig;
use crate::transport::types::AcpError;

fn field<'a>(params: &'a Value, name: &str) -> Result<&'a str, AcpError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, format!("missing {name}")))
}

pub(super) async fn machines(cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let machines = cfg
        .session_resources
        .list_machines()
        .await
        .map_err(super::super::workspace::resource_error)?;
    Ok(json!({"machines": machines}))
}

pub(super) async fn workspaces(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let machine_id = field(params, "machineId")?;
    let workspaces = cfg
        .session_resources
        .list_workspaces(machine_id)
        .await
        .map_err(super::super::workspace::resource_error)?;
    Ok(json!({"workspaces": workspaces}))
}

pub(super) async fn archive_session(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    if !cfg
        .session_manager
        .effective_host_caps()
        .session_workspace_v1
    {
        return Err(AcpError::new(
            -32602,
            "Session workspace capability was not negotiated",
        ));
    }
    let session_id = field(params, "sessionId")?.to_owned();
    let archived = params
        .get("archived")
        .and_then(Value::as_bool)
        .ok_or_else(|| AcpError::new(-32602, "missing archived"))?;
    cfg.session_resources
        .set_session_archived(&session_id, archived)
        .await
        .map_err(super::super::workspace::resource_error)?;
    Ok(json!({"success": true}))
}
