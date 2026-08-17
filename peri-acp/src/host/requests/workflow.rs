//! Workflow 命令 handler：list_runs / kill_agent / kill_run / resume（自
//! requests.rs 拆出，请求分发见 `host/requests.rs`）。

use std::collections::HashMap;

use serde_json::Value;
use tracing::{info, warn};

use super::super::SessionState;
use crate::transport::types::AcpError;

pub(super) fn handle_list_runs(
    params: &Value,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;

    let runs = sessions
        .get(req_session_id)
        .and_then(|s| s.workflow_middleware.as_ref())
        .map(|mw| mw.runs_snapshot())
        .unwrap_or_default();

    let resp = serde_json::json!({ "runs": runs });
    Ok(resp)
}

pub(super) async fn handle_kill_agent(
    params: &Value,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    // 显式按请求 sessionId 查找（与 workflow/list_runs 一致），
    // 多 session 时不得取第一个带 middleware 的 session（issue 2026-08-05）
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let run_id = params
        .get("runId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;
    let agent_id = params
        .get("agentId")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| AcpError::new(-32602, "missing agentId"))?;

    let mw = sessions
        .get(req_session_id)
        .and_then(|s| s.workflow_middleware.as_ref())
        .ok_or_else(|| AcpError::new(-32602, format!("session not found: {req_session_id}")))?;
    let killed = mw.kill_agent(run_id, agent_id).await;

    if killed {
        info!(run_id, agent_id, "Workflow agent killed via ACP");
    } else {
        warn!(run_id, agent_id, "Workflow agent kill failed (not found)");
    }
    Ok(serde_json::json!({ "killed": killed }))
}

pub(super) fn handle_kill_run(
    params: &Value,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    // 显式按请求 sessionId 查找（与 workflow/list_runs 一致），
    // 多 session 时不得取第一个带 middleware 的 session（issue 2026-08-05）
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let run_id = params
        .get("runId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;

    let mw = sessions
        .get(req_session_id)
        .and_then(|s| s.workflow_middleware.as_ref())
        .ok_or_else(|| AcpError::new(-32602, format!("session not found: {req_session_id}")))?;
    let killed = mw.kill_run(run_id);

    if killed {
        info!(run_id, "Workflow run killed via ACP");
    } else {
        warn!(run_id, "Workflow run kill failed (not found)");
    }
    Ok(serde_json::json!({ "killed": killed }))
}

pub(super) async fn handle_resume(
    params: &Value,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    // 显式按请求 sessionId 查找（与 workflow/list_runs、kill_run 一致），
    // 多 session 时不得取第一个带 middleware 的 session（issue 2026-08-05）
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let run_id = params
        .get("runId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;

    let mw = sessions
        .get(req_session_id)
        .and_then(|s| s.workflow_middleware.as_ref())
        .ok_or_else(|| AcpError::new(-32602, format!("session not found: {req_session_id}")))?;

    let new_run_id = mw
        .resume(run_id)
        .await
        .map_err(|e| AcpError::new(-32603, e))?;

    info!(old_run = %run_id, new_run = %new_run_id, "Workflow resumed via ACP");
    Ok(serde_json::json!({
        "runId": new_run_id,
        "resumedFrom": run_id
    }))
}
