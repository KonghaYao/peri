//! 会话配置：set_mode / set_config_option / update_config。

use crate::provider::LlmProvider;
use crate::session::state_builders::{apply_profile_effort, parse_permission_mode};
use agent_client_protocol::{
    schema::v1::{
        SessionId, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
        SetSessionModeRequest, SetSessionModeResponse,
    },
    Client, ConnectionTo, Error, Handled, Responder, UntypedMessage,
};

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
    let session_id = req.session_id.0.as_ref();
    match &req.value {
        agent_client_protocol_schema::v1::SessionConfigOptionValue::ValueId { value } => {
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
                    apply_profile_effort(&ctx.peri_config, v);
                    // 同步更新 LlmProvider（thinking 变更需要重建 provider）
                    let new_provider = {
                        let c = ctx.peri_config.read();
                        LlmProvider::from_config(&c)
                    };
                    if let Some(new_provider) = new_provider {
                        *ctx.provider.write() = new_provider;
                    }
                    // Thinking 变更 → invalidate cached LLM 实例
                    if !session_id.is_empty() {
                        let mut sessions = ctx.sessions.write();
                        if let Some(s) = sessions.get_mut(session_id) {
                            s.agent_pool.invalidate();
                        }
                    }
                    tracing::info!(effort = %v, "Thinking effort changed via configOption");
                }
                "context_1m" => {
                    let enabled = v == "true" || v == "1";
                    {
                        let mut c = ctx.peri_config.write();
                        let alias = c.config.active_alias.clone();
                        if let Some(profile) = c.config.profiles.get_mut(&alias) {
                            profile.context_1m = enabled;
                        }
                    }
                    tracing::info!(enabled = %enabled, "Context 1M changed via configOption");
                }
                _ => {
                    tracing::debug!(config_id = %config_id, "Unknown config option");
                }
            }
        }
        agent_client_protocol_schema::v1::SessionConfigOptionValue::Boolean { value: _ } => {
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

    let new_cfg: crate::provider::PeriConfig = serde_json::from_value(config_val)
        .map_err(|e| Error::invalid_request().data(format!("Invalid config: {e}")))?;

    // Validate providers
    if new_cfg.config.providers.is_empty() {
        return Err(Error::invalid_request().data("providers cannot be empty"));
    }
    // Profile 是唯一事实源：各 profile 引用的 provider 必须存在于 providers
    for alias in crate::provider::Profiles::ALL {
        let pid = new_cfg
            .config
            .profiles
            .get(alias)
            .map(|p| p.provider.as_str())
            .unwrap_or("");
        if !pid.is_empty() && !new_cfg.config.providers.iter().any(|p| p.id == pid) {
            return Err(Error::invalid_request()
                .data(format!("profile {alias}: provider '{pid}' not found")));
        }
    }
    // active_alias 必须是固定档位键（大小写不敏感，与 Profiles::get 行为一致），
    // 否则后续依赖 active_alias 的处理会静默 no-op
    if !crate::provider::Profiles::ALL
        .iter()
        .any(|a| a.eq_ignore_ascii_case(&new_cfg.config.active_alias))
    {
        return Err(Error::invalid_request().data(format!(
            "active_alias '{}' not in Profiles::ALL",
            new_cfg.config.active_alias
        )));
    }

    *ctx.peri_config.write() = new_cfg.clone();

    if let Some(p) = crate::provider::LlmProvider::from_config(&new_cfg) {
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
