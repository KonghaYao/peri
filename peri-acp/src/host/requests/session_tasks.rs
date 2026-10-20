//! Background task query/cancel handlers for the owning session tree.

use std::sync::Arc;

use serde_json::Value;
use tracing::info;

use super::super::AcpServerConfig;
use crate::transport::types::AcpError;

pub(crate) async fn handle_cancel_bg_task(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let task_id = params
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing taskId"))?;

    // 会话不存在时如实报错（此前静默返回 success，掩盖取消未生效）
    let session = cfg
        .session_manager
        .get_session(req_session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session not found: {req_session_id}")))?;
    let manager = Arc::clone(&session.task_manager);
    drop(session);
    let manager = if let Some(child_id) = params.get("childThreadId").and_then(Value::as_str) {
        manager.child_task_manager(child_id).ok_or_else(|| {
            AcpError::new(
                -32602,
                "child resource owner is outside owning session tree",
            )
        })?
    } else {
        manager
    };
    manager
        .cancel_async(task_id)
        .await
        .map_err(|e| AcpError::new(-32603, e.to_string()))?;
    info!(session_id = %req_session_id, task_id = %task_id, "Background task cancelled via ACP");
    Ok(serde_json::json!({ "success": true }))
}

pub(crate) fn handle_bg_tasks(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let session = cfg
        .session_manager
        .get_session(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    let manager = if let Some(child_id) = params.get("childThreadId").and_then(Value::as_str) {
        session
            .task_manager
            .child_task_manager(child_id)
            .ok_or_else(|| {
                AcpError::new(
                    -32602,
                    "child resource owner is outside owning session tree",
                )
            })?
    } else {
        session.task_manager.clone()
    };
    Ok(super::super::task_snapshot_value(manager.snapshot()))
}
