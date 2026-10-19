//! 会话元数据协调器：每 tick 派生标题与运行配置，不直接发布 atom。
//!
//! 运行配置每次从本地配置起算，再由 metadata 覆盖；查询失败清空运行字段，
//! 同一会话保留上次标题，切换或关闭会话清理标题。只缓存标题，不缓存运行配置。

use crate::acp_client::AcpTuiClient;
use crate::app::service_registry::SharedPeriConfig;
use crate::kit::atoms::ProviderSummary;
use peri_acp_types::permission::PermissionMode;

#[derive(Default)]
pub(super) struct SessionMetadata {
    pub title: String,
    pub provider_name: String,
    pub model_alias: String,
    pub model_name: String,
    pub effort: String,
    pub permission_mode: String,
}

impl SessionMetadata {
    pub(super) fn from_config(config: &SharedPeriConfig, permission: PermissionMode) -> Self {
        let (provider_name, model_alias, model_name, effort) = derive_provider_and_model(config);
        Self {
            provider_name,
            model_alias,
            model_name,
            effort,
            permission_mode: permission_mode_label(permission).to_string(),
            ..Default::default()
        }
    }
}

#[derive(Default)]
pub(super) struct SessionMetadataRefresh {
    session_id: String,
    title: String,
}

impl SessionMetadataRefresh {
    pub(super) async fn refresh(
        &mut self,
        client: Option<&AcpTuiClient>,
        session_id: &str,
        mut result: SessionMetadata,
    ) -> SessionMetadata {
        if session_id != self.session_id {
            self.title.clear();
        }
        if !session_id.is_empty()
            && let Some(client) = client
        {
            match client
                .send_raw_request(
                    "session/metadata",
                    serde_json::json!({"sessionId":session_id}),
                )
                .await
            {
                Ok(metadata) => {
                    if let Some(mode) = metadata
                        .get("permissionMode")
                        .and_then(serde_json::Value::as_str)
                    {
                        result.permission_mode = match mode {
                            "accept_edit" => "accept-edit",
                            "auto" => "auto-mode",
                            mode => mode,
                        }
                        .to_string();
                    }
                    if let Some(alias) = metadata
                        .get("modelAlias")
                        .and_then(serde_json::Value::as_str)
                    {
                        result.model_alias = alias.to_string();
                    }
                    result.model_name = metadata
                        .get("modelName")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&result.model_alias)
                        .to_string();
                    result.provider_name = metadata
                        .get("providerName")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    result.effort = metadata
                        .get("effort")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.title = metadata
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                }
                Err(error) => {
                    result = SessionMetadata::default();
                    tracing::warn!(%error, "session metadata lookup failed");
                }
            }
        }
        self.session_id = session_id.to_string();
        result.title = self.title.clone();
        result
    }
}

pub(super) fn derive_provider_and_model(
    config: &SharedPeriConfig,
) -> (String, String, String, String) {
    let cfg = config.read();
    let active_alias = cfg.config.active_alias.clone();
    let profile = cfg.config.profiles.get(&active_alias);
    let provider = profile.and_then(|profile| {
        if profile.provider.is_empty() {
            cfg.config.providers.first()
        } else {
            cfg.config
                .providers
                .iter()
                .find(|provider| provider.id == profile.provider)
        }
    });
    let provider_name = provider
        .map(|provider| provider.provider_type.clone())
        .unwrap_or_else(|| {
            profile
                .map(|profile| profile.provider.clone())
                .unwrap_or_else(|| active_alias.clone())
        });
    let model_name = profile
        .and_then(|profile| profile.model.as_ref())
        .filter(|model| !model.is_empty())
        .cloned()
        .or_else(|| {
            provider
                .and_then(|provider| provider.models.get_model(&active_alias).map(str::to_string))
        })
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| active_alias.clone());
    let effort = profile
        .map(|profile| profile.effort.clone())
        .filter(|effort| !effort.is_empty())
        .unwrap_or_else(|| "xhigh".to_string());
    (provider_name, active_alias, model_name, effort)
}

pub(super) fn derive_providers(config: &SharedPeriConfig) -> Vec<ProviderSummary> {
    let cfg = config.read();
    let active_provider = cfg
        .config
        .profiles
        .get(&cfg.config.active_alias)
        .map(|profile| profile.provider.clone())
        .unwrap_or_default();
    cfg.config
        .providers
        .iter()
        .map(|provider| ProviderSummary {
            id: provider.id.clone(),
            provider_type: provider.provider_type.clone(),
            is_active: provider.id == active_provider,
            has_api_key: !provider.api_key.is_empty(),
            base_url: (!provider.base_url.is_empty()).then(|| provider.base_url.clone()),
        })
        .collect()
}

pub(super) fn permission_mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept-edit",
        PermissionMode::AutoMode => "auto-mode",
        PermissionMode::Bypass => "bypass",
    }
}
