use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, NOTIFICATION, Notification, PERI_CONFIG_HANDLE, PROVIDER_LIST,
    ProviderSummary,
};
use peri_acp::provider::config::{ProviderConfig, ProviderModels};
use std::time::Duration;

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
                    until: peri_time::monotonic_now() + Duration::from_secs(2),
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
    if !crate::kit::config_service::persist_candidate(&snap) {
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
            let has_api_key = !p.api_key.is_empty();
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

/// 删除候选：在副本上移除目标身份，不改内存。
///
/// 返回 `None` 表示目标已不在配置里——调用方据此跳过落盘，重复删除不产生写入。
pub(super) fn delete_candidate(
    cfg: &crate::config::PeriConfig,
    provider_id: &str,
) -> Option<crate::config::PeriConfig> {
    let mut snap = cfg.clone();
    let len_before = snap.config.providers.len();
    snap.config.providers.retain(|p| p.id != provider_id);
    (snap.config.providers.len() < len_before).then_some(snap)
}

/// 删除指定身份的 provider：落盘成功后，才从 PERI_CONFIG_HANDLE 移除 + 刷新 + 推送 ACP。
///
/// 身份（provider id）由调用方在确认门下冻结，这里不再按列表下标重取目标——列表每 tick
/// 重派生，按下标重取会在确认期间漂移到另一个 provider（删除不可逆）。
///
/// 与 [`save_login_edit`] 同一约定：落盘失败时可见列表仍是已持久化的 provider 集合
/// （删除不可逆，不能让界面显示一个重启后复活的删除结果）。
pub(super) fn delete_provider(provider_id: &str) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };

    let candidate = {
        let cfg = handle.read();
        match delete_candidate(&cfg, provider_id) {
            Some(candidate) => candidate,
            None => return,
        }
    };

    if !crate::kit::config_service::persist_candidate(&candidate) {
        return;
    }

    *handle.write() = candidate.clone();
    refresh_provider_list();

    // 推送 ACP
    if let Some(client) = ACP_CLIENT_HANDLE.get() {
        tokio::spawn(async move {
            if let Err(e) = client.update_config(&candidate).await {
                tracing::warn!(error = %e, "LoginPanel: update_config push failed after delete");
            }
        });
    }

    tracing::info!(provider_id = %provider_id, "LoginPanel: provider deleted");
}
