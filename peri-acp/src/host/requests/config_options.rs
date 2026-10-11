//! Session 配置命令 handler：set_mode / set_config_option / update_config
//! 与配置持久化辅助（自 requests.rs 拆出，请求分发见 `host/requests.rs`）。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{SetSessionConfigOptionResponse, SetSessionModeResponse};
use serde_json::Value;
use tracing::{debug, info};

use super::super::notify::{extract_session_id, send_config_option_update};
use super::super::{apply_profile_effort, parse_permission_mode, AcpServerConfig, SessionState};
use crate::dispatch::config_update::make_config_options;
use crate::provider::LlmProvider;
use crate::transport::types::AcpError;

fn expected_revision(
    cfg: &AcpServerConfig,
) -> Result<peri_config::ConfigurationRevision, AcpError> {
    cfg.config_source
        .snapshot()
        .map(|snapshot| snapshot.revision())
        .ok_or_else(|| AcpError::new(-32603, "Configuration authority unavailable"))
}

pub(crate) async fn handle_set_mode(
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let mode_id = params
        .get("modeId")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let session_id = extract_session_id(params, "");
    let mode = parse_permission_mode(mode_id);
    cfg.permission_mode.store(mode);
    info!(mode_id = %mode_id, "Permission mode changed");
    let resp = SetSessionModeResponse::new();
    send_config_option_update(transport.as_ref(), session_id, cfg).await;
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(crate) async fn handle_set_config_option(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let revision = if matches!(
        params.get("configId").and_then(Value::as_str),
        Some("model" | "thinking_effort" | "context_1m")
    ) {
        Some(expected_revision(cfg)?)
    } else {
        None
    };
    let config_id = params
        .get("configId")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_id = extract_session_id(params, "");
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    match config_id {
        "mode" => {
            let mode = parse_permission_mode(value);
            cfg.permission_mode.store(mode);
            info!(mode = %value, "Permission mode changed via configOption");
        }
        "model" | "thinking_effort" | "context_1m" => {
            let candidate = parking_lot::RwLock::new(cfg.peri_config.read().clone());
            match config_id {
                "model" => candidate.write().config.active_alias = value.to_owned(),
                "thinking_effort" => apply_profile_effort(&candidate, value),
                "context_1m" => {
                    let mut config = candidate.write();
                    let alias = config.config.active_alias.clone();
                    let profile = config.config.profiles.get_mut(&alias).ok_or_else(|| {
                        AcpError::new(-32602, "active profile has no usable provider")
                    })?;
                    profile.context_1m = value == "true" || value == "1";
                }
                _ => unreachable!(),
            }
            let candidate = candidate.into_inner();
            let provider =
                LlmProvider::from_config_for_alias(&candidate, &candidate.config.active_alias)
                    .ok_or_else(|| {
                        AcpError::new(-32602, "active profile has no usable provider")
                    })?;
            let accepted = cfg
                .config_source
                .save(
                    revision.expect("persistent option has a revision"),
                    &candidate,
                )
                .map_err(|_| AcpError::new(-32603, "Failed to persist config"))?;
            *cfg.peri_config.write() = accepted.settings().clone();
            *cfg.provider.write() = accepted
                .provider()
                .cloned()
                .map(LlmProvider::from_resolved)
                .unwrap_or(provider);
            if let Some(state) = sessions.get_mut(session_id) {
                state.agent_pool.invalidate();
            }
            info!(config_id = %config_id, value = %value, "Configuration option persisted");
        }
        _ => {
            debug!(config_id = %config_id, "Unknown config option");
        }
    }
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    let resp = SetSessionConfigOptionResponse::new(config_options);
    send_config_option_update(transport.as_ref(), session_id, cfg).await;
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(crate) async fn handle_update_config(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let revision = expected_revision(cfg)?;
    let session_id = extract_session_id(params, "");
    let new_cfg: crate::provider::PeriConfig =
        serde_json::from_value(params.get("config").cloned().unwrap_or_default())
            .map_err(|_| AcpError::new(-32602, "Invalid config"))?;

    if new_cfg.config.providers.is_empty() {
        return Err(AcpError::new(-32602, "providers cannot be empty"));
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
            return Err(AcpError::new(
                -32602,
                format!("profile {alias}: provider '{pid}' not found"),
            ));
        }
    }

    let new_provider =
        LlmProvider::from_config_for_alias(&new_cfg, &new_cfg.config.active_alias)
            .ok_or_else(|| AcpError::new(-32602, "active profile has no usable provider"))?;
    // Each environment owns its model selection; only connection definitions are
    // shared with environments assembled from this exact configuration source.
    // Resolve every candidate before persistence so failure leaves live state intact.
    let mut refreshed = Vec::new();
    for (id, state) in sessions.iter() {
        let Some(environment) = state.environment.as_ref() else {
            continue;
        };
        if !Arc::ptr_eq(&environment.cfg.config_source, &cfg.config_source)
            || Arc::ptr_eq(&environment.cfg.peri_config, &cfg.peri_config)
        {
            continue;
        }
        let mut candidate = environment.cfg.peri_config.read().clone();
        candidate.config.providers = new_cfg.config.providers.clone();
        let provider =
            LlmProvider::from_config_for_alias(&candidate, &candidate.config.active_alias)
                .ok_or_else(|| {
                    AcpError::new(-32602, "existing session profile has no usable provider")
                })?;
        refreshed.push((id.clone(), environment.clone(), candidate, provider));
    }
    let accepted = cfg
        .config_source
        .save(revision, &new_cfg)
        .map_err(|_| AcpError::new(-32603, "Failed to persist config"))?;
    *cfg.peri_config.write() = accepted.settings().clone();
    *cfg.provider.write() = accepted
        .provider()
        .cloned()
        .map(LlmProvider::from_resolved)
        .unwrap_or(new_provider);
    for (_, _, candidate, _) in &mut refreshed {
        candidate.config.providers = accepted.settings().config.providers.clone();
    }
    for (_, environment, candidate, provider) in &refreshed {
        *environment.cfg.peri_config.write() = candidate.clone();
        *environment.cfg.provider.write() = provider.clone();
    }
    for state in sessions.values_mut() {
        if state.environment.as_ref().is_none_or(|environment| {
            Arc::ptr_eq(&environment.cfg.config_source, &cfg.config_source)
        }) {
            state.agent_pool.invalidate();
        }
    }
    for (id, environment, _, _) in &refreshed {
        send_config_option_update(transport.as_ref(), id, &environment.cfg).await;
    }

    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    send_config_option_update(transport.as_ref(), session_id, cfg).await;
    serde_json::to_value(SetSessionConfigOptionResponse::new(config_options))
        .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}
