//! 会话配置：set_mode / set_config_option / update_config。

use agent_client_protocol::{
    schema::{
        SessionId, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
        SetSessionModeRequest, SetSessionModeResponse,
    },
    Client, ConnectionTo, Error, Handled, Responder, UntypedMessage,
};
use peri_acp::session::state_builders::{apply_thinking_effort, parse_permission_mode};

use super::super::{context::StdioContext, model, notification};

/// 处理 session/set_mode
pub(crate) async fn handle_set_mode(
    ctx: &StdioContext,
    req: SetSessionModeRequest,
    responder: Responder<SetSessionModeResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), Error> {
    let mode_id = req.mode_id.0.as_ref();
    let mode = parse_permission_mode(mode_id);
    ctx.permission_mode.store(mode);
    tracing::info!(mode_id = %mode_id, "Permission mode changed");
    let _config_options = notification::send_config_update(ctx, &req.session_id, &cx);
    responder.respond(SetSessionModeResponse::new())
}

/// 处理 session/set_config_option
pub(crate) async fn handle_set_config_option(
    ctx: &StdioContext,
    req: SetSessionConfigOptionRequest,
    responder: Responder<SetSessionConfigOptionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), Error> {
    let config_id = req.config_id.0.as_ref();
    match &req.value {
        agent_client_protocol_schema::SessionConfigOptionValue::ValueId { value } => {
            let v = value.0.as_ref();
            match config_id {
                "mode" => {
                    let mode = parse_permission_mode(v);
                    ctx.permission_mode.store(mode);
                    tracing::info!(mode = %v, "Permission mode changed via configOption");
                }
                "model" => {
                    let _ = model::switch_model(ctx, req.session_id.0.as_ref(), v);
                }
                "thinking_effort" => {
                    apply_thinking_effort(&ctx.peri_config, v);
                    tracing::info!(effort = %v, "Thinking effort changed via configOption");
                }
                _ => {
                    tracing::debug!(config_id = %config_id, "Unknown config option");
                }
            }
        }
        agent_client_protocol_schema::SessionConfigOptionValue::Boolean { value: _ } => {
            tracing::debug!(config_id = %config_id, "Boolean config option not handled");
        }
        _ => {
            tracing::debug!(config_id = %config_id, "Unknown config option value type");
        }
    }
    let config_options = notification::send_config_update(ctx, &req.session_id, &cx);
    responder.respond(SetSessionConfigOptionResponse::new(config_options))
}

/// 处理 session/update_config (custom extension)
pub(crate) async fn handle_update_config(
    ctx: &StdioContext,
    req: UntypedMessage,
    responder: Responder<serde_json::Value>,
    cx: ConnectionTo<Client>,
) -> Result<Handled<(UntypedMessage, Responder<serde_json::Value>)>, Error> {
    // Only handle session/update_config; pass through all others
    if req.method() != "session/update_config" {
        return Ok(Handled::No {
            message: (req, responder),
            retry: false,
        });
    }

    let session_id = req
        .params()
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let config_val = req.params().get("config").cloned().unwrap_or_default();

    let new_cfg: peri_tui::config::PeriConfig = serde_json::from_value(config_val)
        .map_err(|e| Error::invalid_request().data(format!("Invalid config: {e}")))?;

    // Validate providers
    if new_cfg.config.providers.is_empty() {
        return Err(Error::invalid_request().data("providers cannot be empty"));
    }
    let active_pid = new_cfg.config.active_provider_id.as_str();
    if !active_pid.is_empty() && !new_cfg.config.providers.iter().any(|p| p.id == active_pid) {
        return Err(
            Error::invalid_request().data(format!("active_provider_id '{active_pid}' not found"))
        );
    }

    *ctx.peri_config.write() = new_cfg.clone();

    if let Some(p) = peri_tui::app::agent::LlmProvider::from_config(&new_cfg) {
        tracing::info!(
            model = %p.model_name(),
            "Provider updated via session/update_config"
        );
        *ctx.provider.write() = p;
    }

    // Model switch → invalidate cached LLM instances
    if !session_id.is_empty() {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&session_id) {
            s.agent_pool.invalidate();
        }
    }

    let sid = SessionId::new(&*session_id);
    let config_options = notification::send_config_update(ctx, &sid, &cx);
    let resp = serde_json::to_value(SetSessionConfigOptionResponse::new(config_options))
        .map_err(|e| Error::internal_error().data(format!("Serialize failed: {e}")))?;
    let _ = responder.respond(resp);
    Ok(Handled::Yes)
}
