use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, NOTIFICATION, Notification, PERI_CONFIG_HANDLE, PROVIDER_LIST,
    ProviderSummary,
};
use peri_acp::provider::config::{PeriConfig, ProviderAuthConfig};
use std::time::Duration;

use super::edit_state::{LoginEditState, LoginError, LoginField};
use crate::kit::config_service::DraftSaveError;

/// 保存编辑结果：构造候选 → 显式 expected revision 落盘 → 只发布 accepted 快照。
///
/// 返回 `true` 表示保存成功；`false` 表示校验失败、revision 冲突或持久化失败
/// （不退出编辑，防止“假保存成功”）。失败时草稿保留，字段错误写入 `inline_error`。
pub(super) fn save_login_edit(es: &mut LoginEditState) -> bool {
    // 输入级错误（例如多行粘贴被清洗）没有可重算的来源，错误未清除时不得提交；
    // 字段校验错误同样保留到用户修正为止。文本编辑、choice 切换与干净粘贴会清除
    // 错误，因此这里不会形成永久阻断。
    if let Some(error) = &es.inline_error {
        notify_validation_error(&error.message);
        return false;
    }
    // 先做字段定位校验：错误必须在检查配置源之前就能显示（不依赖全局句柄）。
    let candidate = match es.build_candidate() {
        Ok(candidate) => candidate,
        Err(error) => {
            notify_validation_error(&error.message);
            es.inline_error = Some(error);
            return false;
        }
    };
    if PERI_CONFIG_HANDLE.get().is_none() {
        es.inline_error = Some(LoginError {
            field: LoginField::Confirm,
            message: i18n::tr("login-error-authority"),
            alias: None,
            kind: super::edit_types::LoginErrorKind::Field,
        });
        return false;
    }
    let Some(revision) = es.expected_revision else {
        es.inline_error = Some(LoginError {
            field: LoginField::Confirm,
            message: i18n::tr("login-error-authority"),
            alias: None,
            kind: super::edit_types::LoginErrorKind::Field,
        });
        return false;
    };

    match crate::kit::config_service::persist_draft(&candidate, revision) {
        Ok(accepted) => {
            publish_accepted(accepted);
            if es.is_new() {
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
        Err(DraftSaveError::Conflict) => {
            es.inline_error = Some(LoginError::conflict(i18n::tr("config-draft-conflict")));
            false
        }
        Err(DraftSaveError::Failed(_)) => {
            // 普通保存失败已由 config_service 写入 NOTIFICATION；草稿与焦点保持不变。
            false
        }
    }
}

fn notify_validation_error(message: &str) {
    *NOTIFICATION.state().write() = Some(Notification {
        message: message.to_string(),
        until: peri_time::monotonic_now() + Duration::from_secs(2),
    });
}

/// 发布 authority 接纳后的配置：内存句柄、Provider 投影、ACP 运行端。
///
/// ACP 推送失败只提示“已保存但同步失败”，不回滚已落盘配置，也不发布原始 candidate。
fn publish_accepted(accepted: PeriConfig) {
    if let Some(handle) = PERI_CONFIG_HANDLE.get() {
        *handle.write() = accepted.clone();
    }
    refresh_provider_list(&accepted);
    push_config_to_acp(accepted);
}

fn push_config_to_acp(snapshot: PeriConfig) {
    if let Some(client) = ACP_CLIENT_HANDLE.get() {
        tokio::spawn(async move {
            if let Err(error) = client.update_config(&snapshot).await {
                crate::kit::config_service::notify_runtime_sync_failed(&anyhow::anyhow!(
                    error.to_string()
                ));
            }
        });
    }
}

/// 从给定配置刷新 PROVIDER_LIST atom（发布值必须来自 accepted snapshot）。
pub(super) fn refresh_provider_list(cfg: &PeriConfig) {
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
        .map(|p| ProviderSummary {
            id: p.id.clone(),
            provider_type: p.provider_type.clone(),
            is_active: p.id == active_profile_provider,
            has_api_key: !p.api_key.is_empty(),
            has_chatgpt_plan: matches!(&p.auth, Some(ProviderAuthConfig::ChatGptPlan { .. })),
            base_url: (!p.base_url.is_empty()).then(|| p.base_url.clone()),
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

    // 删除是即时动作：沿用“保存时取最新 revision”的入口语义。
    if !crate::kit::config_service::persist_candidate(&candidate) {
        return;
    }

    publish_accepted(candidate);
    tracing::info!(provider_id = %provider_id, "LoginPanel: provider deleted");
}
