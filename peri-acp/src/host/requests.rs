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
mod workflow;

pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    match method {
        "initialize" => session_lifecycle::handle_initialize(params, cfg),
        "session/new" => session_lifecycle::handle_new(params, cfg, sessions, transport).await,
        "session/set_mode" => config_options::handle_set_mode(params, cfg, transport).await,
        "session/set_config_option" => {
            config_options::handle_set_config_option(params, cfg, sessions, transport).await
        }
        "session/load" => session_lifecycle::handle_load(params, cfg, sessions, transport).await,
        "session/list" => session_lifecycle::handle_list(params, cfg).await,
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
    }
}

#[cfg(test)]
#[path = "requests_test.rs"]
mod tests;
