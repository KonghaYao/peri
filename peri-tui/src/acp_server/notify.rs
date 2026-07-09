//! ACP Notification dispatch — handles incoming notifications and pushes
//! session update notifications. Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;

use agent_client_protocol::schema::v1::{AvailableCommandsUpdate, SessionUpdate};
use peri_acp::dispatch::config_update;
use peri_middlewares::skills::SkillMetadata;
use serde_json::Value;
use tracing::{debug, info};

use super::{AcpServerConfig, SessionState};
use crate::app::agent::LlmProvider;

// ── Notification dispatch ────────────────────────────────────────────────────

pub(crate) fn handle_notification(
    method: &str,
    params: &Value,
    sessions: &HashMap<String, SessionState>,
    cfg: &AcpServerConfig,
) {
    match method {
        "session/cancel" => {
            let session_id = extract_session_id(params, "");
            if let Some(state) = sessions.get(session_id)
                && let Some(ref token) = state.cancel_token
            {
                token.cancel();
                info!(session_id = %session_id, "Cancel requested");
            }
        }
        "session/config_update" => {
            // Two formats:
            // 1. {"config": PeriConfig} — full config replace (from update_config)
            // 2. {"configId": "model"/"provider", "value": "..."} — partial (from set_config_option)
            if let Some(config_val) = params.get("config") {
                let new_cfg: crate::config::PeriConfig = match serde_json::from_value(
                    config_val.clone(),
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "config_update notification: invalid config");
                        return;
                    }
                };
                tracing::info!(
                    active_provider = %new_cfg.config.active_provider_id,
                    provider_count = new_cfg.config.providers.len(),
                    "config_update notification: full config replace"
                );
                *cfg.peri_config.write() = new_cfg.clone();
                if let Some(p) = LlmProvider::from_config(&new_cfg) {
                    *cfg.provider.write() = p;
                }
            } else if let (Some(config_id), Some(value)) = (
                params.get("configId").and_then(|v| v.as_str()),
                params.get("value").and_then(|v| v.as_str()),
            ) {
                match config_id {
                    "model" => {
                        let mut c = cfg.peri_config.write();
                        c.config.active_alias = value.to_string();
                        drop(c);
                        let new_provider = {
                            let c = cfg.peri_config.read();
                            LlmProvider::from_config_for_alias(&c, value)
                        };
                        if let Some(p) = new_provider {
                            tracing::info!(alias = %value, "config_update notification: model changed");
                            *cfg.provider.write() = p;
                        }
                    }
                    other => {
                        tracing::debug!(config_id = %other, "config_update notification: unhandled configId");
                    }
                }
            } else {
                tracing::debug!("config_update notification: missing config/configId");
            }
            // No sessions to invalidate — pool will be built fresh on next session/new
        }
        _ => {
            debug!(method = %method, "Unhandled notification");
        }
    }
}

// ── Notification helpers ───────────────────────────────────────────────────────

/// Extract `sessionId` from JSON-RPC params, returning `default_value` if absent.
pub(crate) fn extract_session_id<'a>(params: &'a Value, default_value: &'a str) -> &'a str {
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or(default_value)
}

/// Build the current set of config options and push a `ConfigOptionUpdate` notification.
pub(crate) async fn send_config_option_update(
    transport: &dyn peri_acp::transport::AcpTransport,
    session_id: &str,
    cfg: &AcpServerConfig,
) {
    if session_id.is_empty() {
        return;
    }
    let update = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        SessionUpdate::ConfigOptionUpdate(config_update::make_config_option_update(
            &c,
            &p,
            cfg.permission_mode.load(),
        ))
    };
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize ConfigOptionUpdate");
            return;
        }
    };
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}

/// Push an `AvailableCommandsUpdate` notification for the given session.
pub(crate) async fn send_available_commands_update(
    transport: &dyn peri_acp::transport::AcpTransport,
    session_id: &str,
    skills: &[SkillMetadata],
) {
    if session_id.is_empty() {
        return;
    }
    let commands = peri_acp::dispatch::build_available_commands(skills);
    let meta = skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
    let update = SessionUpdate::AvailableCommandsUpdate(
        AvailableCommandsUpdate::new(commands).meta(
            serde_json::json!({"skillNames": meta})
                .as_object()
                .unwrap()
                .clone(),
        ),
    );
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize AvailableCommandsUpdate");
            return;
        }
    };
    // Use {"update": ..., "sessionId": ...} format — same as TransportEventSink —
    // so that handle_session_update_peri on the TUI side can parse via params.get("update").
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}

/// Push a `SessionInfoUpdate` notification after prompt/compact completes.
pub(crate) async fn send_session_info_update(
    transport: &dyn peri_acp::transport::AcpTransport,
    session_id: &str,
) {
    use agent_client_protocol::schema::v1::SessionInfoUpdate;
    let info = SessionInfoUpdate::new().updated_at(chrono::Utc::now().to_rfc3339());
    let update = SessionUpdate::SessionInfoUpdate(info);
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize SessionInfoUpdate");
            return;
        }
    };
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}
