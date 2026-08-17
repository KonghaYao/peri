//! Session 配置命令 handler：set_mode / set_config_option / update_config
//! 与配置持久化辅助（自 requests.rs 拆出，请求分发见 `host/requests.rs`）。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{SetSessionConfigOptionResponse, SetSessionModeResponse};
use serde_json::Value;
use tracing::{debug, info, warn};

use super::super::notify::{extract_session_id, send_config_option_update};
use super::super::{apply_profile_effort, parse_permission_mode, AcpServerConfig, SessionState};
use crate::dispatch::config_update::make_config_options;
use crate::provider::LlmProvider;
use crate::transport::types::AcpError;

fn persist_config(cfg: &AcpServerConfig) {
    let c = cfg.peri_config.read();
    // 写回当前生效层：路径决策在 ConfigSource 加载时一次性确定（工作区存在则
    // 分层写回工作区，否则写全局），与读取完全对称，不存在第二套实现。
    if let Err(e) = cfg.config_source.save(&c) {
        tracing::warn!(error = %e, "Failed to persist config");
    }
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
        "model" => {
            {
                let mut c = cfg.peri_config.write();
                c.config.active_alias = value.to_string();
            }
            let new_provider = {
                let c = cfg.peri_config.read();
                LlmProvider::from_config_for_alias(&c, value)
            };
            if let Some(new_provider) = new_provider {
                info!(model_id = %value, model = %new_provider.model_name(), "Model changed via configOption");
                *cfg.provider.write() = new_provider;
            }
            // Model switch → invalidate cached LLM instances
            if let Some(s) = sessions.get_mut(session_id) {
                s.agent_pool.invalidate();
            }
            persist_config(cfg);
        }
        "thinking_effort" => {
            apply_profile_effort(&cfg.peri_config, value);
            // 同步更新 LlmProvider（thinking 变更需要重建 provider）
            let new_provider = {
                let c = cfg.peri_config.read();
                LlmProvider::from_config(&c)
            };
            if let Some(new_provider) = new_provider {
                *cfg.provider.write() = new_provider;
            }
            // Thinking 变更 → invalidate cached LLM 实例
            if let Some(s) = sessions.get_mut(session_id) {
                s.agent_pool.invalidate();
            }
            persist_config(cfg);
            info!(effort = %value, "Thinking effort changed via configOption");
        }
        "context_1m" => {
            let enabled = value == "true" || value == "1";
            let mut updated = false;
            {
                let mut c = cfg.peri_config.write();
                let alias = c.config.active_alias.clone();
                if let Some(profile) = c.config.profiles.get_mut(&alias) {
                    profile.context_1m = enabled;
                    updated = true;
                }
            }
            if updated {
                persist_config(cfg);
                info!(enabled = %enabled, "Context 1M changed via configOption (persisted)");
            } else {
                warn!(enabled = %enabled, "Context 1M configOption skipped: active profile not found");
            }
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
    let session_id = extract_session_id(params, "");
    let new_cfg: crate::provider::PeriConfig =
        serde_json::from_value(params.get("config").cloned().unwrap_or_default())
            .map_err(|e| AcpError::new(-32602, format!("Invalid config: {e}")))?;

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

    *cfg.peri_config.write() = new_cfg.clone();

    if let Some(p) = LlmProvider::from_config(&new_cfg) {
        tracing::debug!(
            provider = %p.display_name(),
            model = %p.model_name(),
            "update_config: provider updated"
        );
        *cfg.provider.write() = p;
    } else {
        let active_profile_provider = new_cfg
            .config
            .profiles
            .get(&new_cfg.config.active_alias)
            .map(|p| p.provider.as_str())
            .unwrap_or("");
        tracing::warn!(
            active_provider = %active_profile_provider,
            active_alias = %new_cfg.config.active_alias,
            providers = new_cfg.config.providers.len(),
            "update_config: LlmProvider::from_config returned None, provider NOT updated"
        );
    }

    // Model switch → invalidate cached LLM instances (Main Agent + SubAgent)
    if let Some(s) = sessions.get_mut(session_id) {
        s.agent_pool.invalidate();
    }

    persist_config(cfg);

    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    send_config_option_update(transport.as_ref(), session_id, cfg).await;
    serde_json::to_value(SetSessionConfigOptionResponse::new(config_options))
        .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}
