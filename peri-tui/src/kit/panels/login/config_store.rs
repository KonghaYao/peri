use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, NOTIFICATION, Notification, PERI_CONFIG_HANDLE, PROVIDER_LIST,
    ProviderSummary,
};
use fluent_bundle::FluentValue;
use peri_acp::provider::config::{ProviderConfig, ProviderModels};
use std::time::{Duration, Instant};

use super::LoginEditState;

/// 保存编辑结果：先持久化到磁盘，成功后才发布到 PERI_CONFIG_HANDLE /
/// PROVIDER_LIST / ACP。返回 `true` 表示保存成功；`false` 表示校验失败、
/// 配置句柄缺失或持久化失败（不退出编辑，防止"假保存成功"）。
pub(super) fn save_login_edit(es: &LoginEditState) -> bool {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return false;
    };

    let is_new = es.original_provider_id.is_empty();

    // 构建 detached 配置快照（在副本上修改，落盘成功前不动全局 handle）
    let snap = {
        let mut cfg = handle.write().clone();

        if is_new {
            // New 路径：校验 provider_id 非空后 push 新 ProviderConfig，自动激活
            if es.provider_id.trim().is_empty() {
                *NOTIFICATION.state().write() = Some(Notification {
                    message: i18n::tr("app-provider-name-empty"),
                    until: Instant::now() + Duration::from_secs(2),
                });
                return false;
            }
            let new_config = ProviderConfig {
                provider_type: es.provider_type.clone(),
                id: es.provider_id.clone(),
                api_key: es.api_key.clone(),
                base_url: es.base_url.clone(),
                models: ProviderModels {
                    fable: es.fable_model.clone(),
                    opus: es.opus_model.clone(),
                    sonnet: es.sonnet_model.clone(),
                    haiku: es.haiku_model.clone(),
                },
                ..Default::default()
            };
            cfg.config.providers.push(new_config);
            // 激活：写入 active profile 的 provider
            let alias = cfg.config.active_alias.clone();
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.provider = es.provider_id.clone();
            }
        } else {
            // Edit 路径：查找并更新已有 provider
            if let Some(provider) = cfg
                .config
                .providers
                .iter_mut()
                .find(|p| p.id == es.original_provider_id)
            {
                provider.provider_type = es.provider_type.clone();
                provider.id = es.provider_id.clone();
                provider.api_key = es.api_key.clone();
                provider.base_url = es.base_url.clone();
                provider.models.fable = es.fable_model.clone();
                provider.models.opus = es.opus_model.clone();
                provider.models.sonnet = es.sonnet_model.clone();
                provider.models.haiku = es.haiku_model.clone();

                // 如果 id 变化且该 provider 是当前激活的，同步更新 active profile 的 provider
                let active_profile_provider = cfg
                    .config
                    .profiles
                    .get(&cfg.config.active_alias)
                    .map(|p| p.provider.clone())
                    .unwrap_or_default();
                if active_profile_provider == es.original_provider_id
                    && es.provider_id != es.original_provider_id
                {
                    let alias = cfg.config.active_alias.clone();
                    if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                        profile.provider = es.provider_id.clone();
                    }
                }
            }
        }

        cfg
    };

    // 先持久化：失败则不发布任何变更（handle / PROVIDER_LIST / ACP 均不动）
    if let Err(e) = crate::config::save_effective(&snap) {
        *NOTIFICATION.state().write() = Some(Notification {
            message: i18n::tr_args(
                "config-save-failed",
                &[(
                    "error".to_string(),
                    FluentValue::from(e.to_string().as_str()),
                )],
            ),
            until: Instant::now() + Duration::from_secs(2),
        });
        return false;
    }

    // 持久化成功后：发布到全局 handle + 刷新 PROVIDER_LIST + 推送 ACP
    *handle.write() = snap.clone();
    refresh_provider_list();

    if let Some(client) = ACP_CLIENT_HANDLE.get() {
        tokio::spawn(async move {
            if let Err(e) = client.update_config(&snap).await {
                tracing::warn!(error = %e, "LoginPanel: update_config push failed");
            }
        });
    }

    *NOTIFICATION.state().write() = Some(Notification {
        message: i18n::tr("config-saved").to_string(),
        until: Instant::now() + Duration::from_secs(1),
    });

    if is_new {
        tracing::info!(
            provider_id = %es.provider_id,
            "LoginPanel: new provider created"
        );
    } else {
        tracing::info!(
            provider_id = %es.original_provider_id,
            "LoginPanel: provider edit saved"
        );
    }
    true
}

/// 从 PERI_CONFIG_HANDLE 刷新 PROVIDER_LIST atom（避免多处重复 25 行）
fn refresh_provider_list() {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let cfg = handle.read();
    let active_profile_provider = cfg
        .config
        .profiles
        .get(&cfg.config.active_alias)
        .map(|p| p.provider.clone())
        .unwrap_or_default();
    let updated_providers: Vec<ProviderSummary> = cfg
        .config
        .providers
        .iter()
        .map(|p| {
            let env_key = format!("{}_API_KEY", p.provider_type.to_uppercase());
            let has_api_key = !p.api_key.is_empty() || std::env::var(env_key).is_ok();
            let base_url = if p.base_url.is_empty() {
                None
            } else {
                Some(p.base_url.clone())
            };
            ProviderSummary {
                id: p.id.clone(),
                provider_type: p.provider_type.clone(),
                is_active: p.id == active_profile_provider,
                has_api_key,
                base_url,
            }
        })
        .collect();
    *PROVIDER_LIST.state().write() = updated_providers;
}

/// 持久化 PeriConfig 快照并显示通知
fn persist_and_notify(snap: &crate::config::PeriConfig) {
    match crate::config::save_effective(snap) {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: Instant::now() + Duration::from_secs(1),
            });
        }
        Err(e) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr_args(
                    "config-save-failed",
                    &[(
                        "error".to_string(),
                        FluentValue::from(e.to_string().as_str()),
                    )],
                ),
                until: Instant::now() + Duration::from_secs(2),
            });
        }
    }
}

/// 删除当前选中的 provider：从 PERI_CONFIG_HANDLE 移除 + 刷新 + 持久化 + 推送 ACP
pub(super) fn delete_provider(selected_index: usize) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };

    let provider_id = {
        let provider_state = PROVIDER_LIST.state();
        let store_read = provider_state.read();
        match store_read.get(selected_index) {
            Some(p) => p.id.clone(),
            None => return,
        }
    };

    let removed = {
        let mut cfg = handle.write();
        let len_before = cfg.config.providers.len();
        cfg.config.providers.retain(|p| p.id != provider_id);
        let removed = cfg.config.providers.len() < len_before;
        let snap = cfg.clone();
        drop(cfg);

        if removed {
            refresh_provider_list();
            persist_and_notify(&snap);

            // 推送 ACP
            if let Some(client) = ACP_CLIENT_HANDLE.get() {
                tokio::spawn(async move {
                    if let Err(e) = client.update_config(&snap).await {
                        tracing::warn!(error = %e, "LoginPanel: update_config push failed after delete");
                    }
                });
            }
        }

        removed
    };

    if removed {
        tracing::info!(provider_id = %provider_id, "LoginPanel: provider deleted");
    }
}
