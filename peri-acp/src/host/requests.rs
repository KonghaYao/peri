//! ACP Request dispatch — handles all ACP protocol request methods.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::transport::types::AcpError;
#[cfg(test)]
use peri_acp_types::PeriCaps;

use super::{AcpServerConfig, SessionState};

pub(crate) mod config_options;
mod mcp_oauth;
mod plugin;
mod rewind;
pub(crate) mod session_lifecycle;
mod user_input;
mod workflow;

pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(Value::as_str);
    let lifecycle = matches!(
        method,
        "session/new"
            | "session/load"
            | "session/resume"
            | "session/fork"
            | "session/close"
            | "session/delete"
    );
    let environment = session_id
        .and_then(|id| sessions.get(id))
        .and_then(|state| state.environment.clone());
    let cfg = if lifecycle {
        cfg
    } else {
        environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg)
    };
    if matches!(
        method,
        "session/rewind"
            | "session/set_mode"
            | "session/set_config_option"
            | "session/rename"
            | "workflow/resume"
            | "workflow/kill_agent"
            | "workflow/kill_run"
            | "session/cancel-bg-task"
            | "session/input/enqueue"
            | "session/input/dispatch"
            | "session/input/takeback"
    ) {
        let id = session_id.ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
        if let Some(state) = sessions.get(id) {
            super::workspace::require_owner(state)?;
        } else if method != "session/rename" {
            return Err(AcpError::new(-32602, "session not found"));
        }
    }
    if method == "session/rename" && params.get("title").and_then(Value::as_str).is_none() {
        return Err(AcpError::new(-32602, "missing title"));
    }
    if method == "workflow/resume" {
        super::workspace::validate_expected(cfg, session_id.expect("checked"), None).await?;
    }
    // Renaming an unloaded session is a short mutation lease; never steals a live owner.
    let transient_owner =
        if method == "session/rename" && session_id.is_some_and(|id| !sessions.contains_key(id)) {
            Some(
                cfg.controller
                    .sessions()
                    .acquire_execution_lease(&session_id.expect("checked").to_owned())
                    .await
                    .map_err(super::workspace::workspace_error)?,
            )
        } else {
            None
        };
    let result = match method {
        "initialize" => session_lifecycle::handle_initialize(params, cfg),
        "session/new" => session_lifecycle::handle_new(params, cfg, sessions).await,
        "session/set_mode" => config_options::handle_set_mode(params, cfg, transport).await,
        "session/set_config_option" => {
            config_options::handle_set_config_option(params, cfg, sessions, transport).await
        }
        "session/load" => session_lifecycle::handle_load(params, cfg, sessions, transport).await,
        "peri/session_reset_dirty" => session_lifecycle::handle_reset_dirty(params, cfg).await,
        "session/list" => session_lifecycle::handle_list(params, cfg).await,
        "peri/session_context" => session_lifecycle::handle_context(params, cfg).await,
        "session/metadata" => session_lifecycle::handle_metadata(params, cfg, false).await,
        "peri/session_history" => session_lifecycle::handle_metadata(params, cfg, true).await,
        "session/input/enqueue"
        | "session/input/dispatch"
        | "session/input/takeback"
        | "session/input/snapshot" => {
            user_input::handle_user_input(method, params, cfg, sessions, transport)
        }
        "workflow/list_runs" => workflow::handle_list_runs(params, sessions),
        "workflow/kill_agent" => workflow::handle_kill_agent(params, sessions).await,
        "workflow/kill_run" => workflow::handle_kill_run(params, sessions),
        "workflow/resume" => workflow::handle_resume(params, sessions).await,
        "session/cancel-bg-task" => session_lifecycle::handle_cancel_bg_task(params, cfg),
        "session/close" => session_lifecycle::handle_close(params, cfg, sessions).await,
        "session/delete" => session_lifecycle::handle_delete(params, cfg, sessions).await,
        "session/resume" => {
            session_lifecycle::handle_resume(params, cfg, sessions, transport).await
        }
        "session/fork" => session_lifecycle::handle_fork(params, cfg, sessions, transport).await,
        "session/update_config" => {
            config_options::handle_update_config(params, cfg, sessions, transport).await
        }
        "plugin/install" => plugin::handle_install(params, cfg, sessions, transport).await,
        "plugin/uninstall" => plugin::handle_uninstall(params, cfg, sessions, transport).await,
        "plugin/toggle" => plugin::handle_toggle(params, cfg, transport).await,
        "plugin/search" => plugin::handle_search(params, cfg, transport).await,
        "plugin/list" => plugin::handle_session_snapshot(cfg),
        "plugin/update" => plugin::handle_update(params, cfg, transport).await,
        "session/rename" => session_lifecycle::handle_rename(params, cfg, transport).await,
        "session/rewind-candidates" => rewind::handle_rewind_candidates(params, cfg, sessions),
        "session/rewind-preview" => rewind::handle_rewind_preview(params, cfg, sessions).await,
        "session/rewind" => rewind::handle_rewind(params, cfg, sessions, transport).await,
        "marketplace/refresh" => plugin::handle_refresh(params, cfg).await,
        "mcp/list" => mcp_oauth::handle_list(params, cfg),
        "mcp/oauth_start" => mcp_oauth::handle_oauth_start(params, cfg),
        "mcp/oauth_callback" => mcp_oauth::handle_oauth_callback(params, cfg),
        "mcp/oauth_cancel" => mcp_oauth::handle_oauth_cancel(params, cfg),
        _ => Err(AcpError::new(-32601, format!("Method not found: {method}"))),
    };
    if let Some(owner) = transient_owner {
        owner
            .mark_clean()
            .await
            .map_err(super::workspace::workspace_error)?;
    }
    result
}

#[cfg(test)]
#[path = "requests_test.rs"]
mod tests;
