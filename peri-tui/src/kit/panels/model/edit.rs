//! Model edit actions and their common save/publication boundary.
//!
//! The shared PeriConfig remains the only mutable configuration owner. Each
//! action builds its candidate on a clone, persists it, and only a successful
//! save commits the handle, the local projection and the ACP push — a failed
//! save must not leave the panel showing a switch that never reached disk.

use super::{
    FIELD_CONTEXT_1M, FIELD_EFFORT, FIELD_MAX_TOKENS, FIELD_MODEL, FIELD_PROVIDER, PROFILE_KEYS,
};
use crate::config::PeriConfig;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, MODEL_HIGHLIGHT_UNTIL, NOTIFICATION, Notification, PERI_CONFIG_HANDLE,
};
use fluent_bundle::FluentValue;
use std::sync::Arc;
use std::time::Duration;

/// 宿主配置句柄（面板动作的提交目标；进程级固定，不随活动会话变化）。
type PeriHandle = Arc<parking_lot::RwLock<PeriConfig>>;

/// Effort 五级
const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// Max tokens 预设
const MAX_TOKEN_PRESETS: &[u32] = &[4096, 8192, 16000, 32000, 64000];

/// 切换左侧光标指向的档位为 active profile（先落盘、成功才切换内存并推送 ACP）。
/// pub(crate)：状态栏模型快速切换弹窗复用此切换逻辑。
pub(crate) fn switch_active_alias(idx: usize) {
    let Some(key) = PROFILE_KEYS.get(idx) else {
        return;
    };
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let candidate = {
        let mut candidate = handle.read().clone();
        candidate.config.active_alias = key.to_string();
        candidate
    };
    if commit_snapshot(handle, candidate, ModelChange::ActiveAlias(key.to_string())) {
        tracing::info!(alias = key, "ModelPanel: active_alias switched");
    }
}

/// 编辑右侧字段（forward=true 前进 / false 后退）。先落盘、成功才切换内存并推送 ACP。
pub(super) fn edit_field(alias: String, field: usize, forward: bool) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    // 候选镜像：所有写入先落在副本上，落盘成功前不动句柄。
    let mut cfg = handle.read().clone();

    // 先读取当前值（不可变，纯 clone），避免跨 guard 的字段级借用冲突
    let provider_ids: Vec<String> = cfg.config.providers.iter().map(|p| p.id.clone()).collect();
    let profile_provider = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.provider.clone())
        .unwrap_or_default();
    // 当前显示的模型名：profile.model 未手动设置时回退到 provider 同档位映射，
    // 否则 FIELD_MODEL 定位 idx 落空（unwrap_or(0)）导致首次 → 恰好选中 fallback
    // 显示值、视觉上"切换未生效"。
    let current_model = cfg
        .config
        .profiles
        .get(&alias)
        .and_then(|p| p.model.clone())
        .or_else(|| {
            cfg.config
                .providers
                .iter()
                .find(|p| p.id == profile_provider)
                .and_then(|p| p.models.get_model(&alias))
                .map(str::to_string)
        })
        .unwrap_or_default();
    let current_effort = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.effort.clone())
        .unwrap_or_else(|| "xhigh".to_string());
    let current_max = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.max_tokens)
        .unwrap_or(32000);
    let current_ctx = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.context_1m)
        .unwrap_or(false);

    match field {
        FIELD_PROVIDER => {
            if provider_ids.is_empty() {
                return;
            }
            let idx = provider_ids
                .iter()
                .position(|i| *i == profile_provider)
                .unwrap_or(0);
            let next = provider_ids
                [(idx + if forward { 1 } else { provider_ids.len() - 1 }) % provider_ids.len()]
            .clone();
            // 联动：目标 provider 同档位映射 → 覆盖 profile.model；无映射 → None 触发回退
            let mapped = cfg
                .config
                .providers
                .iter()
                .find(|p| p.id == next)
                .and_then(|p| p.models.get_model(&alias))
                .map(str::to_string)
                .filter(|m| !m.is_empty());
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.provider = next;
                profile.model = mapped;
            }
        }
        FIELD_MODEL => {
            let provider = cfg
                .config
                .providers
                .iter()
                .find(|p| p.id == profile_provider);
            let Some(provider) = provider else {
                return;
            };
            // 候选 = provider 四个档位的全部模型名（去空、去重）+ 当前手动模型保底；
            // 直接读字段而非 get_model，避免 fable 空回退 opus 造成重复
            let mut models: Vec<String> = Vec::new();
            for tier_model in [
                &provider.models.opus,
                &provider.models.sonnet,
                &provider.models.haiku,
                &provider.models.fable,
            ] {
                if !tier_model.is_empty() && !models.contains(tier_model) {
                    models.push(tier_model.clone());
                }
            }
            if !models.contains(&current_model) && !current_model.is_empty() {
                models.insert(0, current_model.clone());
            }
            if models.is_empty() {
                return;
            }
            let idx = models.iter().position(|m| *m == current_model).unwrap_or(0);
            let next =
                models[(idx + if forward { 1 } else { models.len() - 1 }) % models.len()].clone();
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.model = Some(next);
            }
        }
        FIELD_EFFORT => {
            let cur = EFFORT_LEVELS
                .iter()
                .position(|e| *e == current_effort)
                .unwrap_or(0);
            let next = EFFORT_LEVELS
                [(cur + if forward { 1 } else { EFFORT_LEVELS.len() - 1 }) % EFFORT_LEVELS.len()]
            .to_string();
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.effort = next;
            }
        }
        FIELD_MAX_TOKENS => {
            let cur = MAX_TOKEN_PRESETS
                .iter()
                .position(|v| *v == current_max)
                .unwrap_or(0);
            let next = MAX_TOKEN_PRESETS[(cur
                + if forward {
                    1
                } else {
                    MAX_TOKEN_PRESETS.len() - 1
                })
                % MAX_TOKEN_PRESETS.len()];
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.max_tokens = next;
            }
        }
        FIELD_CONTEXT_1M => {
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.context_1m = !current_ctx;
            }
        }
        _ => return,
    }
    commit_snapshot(handle, cfg, ModelChange::ProfileField(alias));
}

/// Describes only the immediate display changes made by this action.
/// Inactive profile edits still persist and push the complete configuration.
enum ModelChange {
    ActiveAlias(String),
    ProfileField(String),
}

/// 提交边界：**保存成功才提交**内存、本地投影与 ACP 推送。
///
/// 保存失败时句柄保持已持久化的值（面板不会显示未落盘的切换），也不推送未落盘
/// 的配置；失败经 `NOTIFICATION` 提示原因。
/// 返回是否已持久化——调用方据此决定日志等本地副作用。
fn commit_snapshot(handle: &PeriHandle, candidate: PeriConfig, change: ModelChange) -> bool {
    let result = crate::config::save_effective(&candidate);
    let persisted = result.is_ok();
    notify_save_result(result);
    if !persisted {
        return false;
    }
    *handle.write() = candidate.clone();
    project_model_change(&candidate, change);
    tokio::spawn(async move {
        if let Some(client) = ACP_CLIENT_HANDLE.get()
            && let Err(e) = client.update_config(&candidate).await
        {
            tracing::warn!(error = %e, "ModelPanel: update_config push failed");
        }
    });
    true
}

fn project_model_change(snap: &PeriConfig, change: ModelChange) {
    if ACP_CLIENT_HANDLE
        .get()
        .is_some_and(|client| client.has_session())
    {
        return;
    }
    let (alias, switch_active) = match change {
        ModelChange::ActiveAlias(alias) => (alias, true),
        ModelChange::ProfileField(alias) => (alias, false),
    };
    let resolved = resolve_model_name_for_alias(&snap.config, &alias);
    // 只提交本次确实改动的字段：整快照 clone 后回写会覆盖同期的进程/服务投影
    // （方案 §7.2 禁止分别 clone 后替换整个快照）。
    let mut update = crate::kit::service_snapshot::SessionFieldUpdate::default();
    if switch_active {
        update.model_alias = Some(alias.clone());
    }
    if switch_active || alias == snap.config.active_alias {
        update.model_name = Some(resolved);
        update.effort = Some(
            snap.config
                .profiles
                .get(&alias)
                .map(|p| p.effort.clone())
                .unwrap_or_else(|| "xhigh".to_string()),
        );
        // Preserve the immediate projection contract: alias switching leaves
        // provider_name for the periodic snapshot; only active edits update it.
        if !switch_active {
            let provider_type = snap
                .config
                .profiles
                .get(&alias)
                .and_then(|pf| snap.config.providers.iter().find(|p| p.id == pf.provider))
                .map(|p| p.provider_type.clone())
                .unwrap_or_default();
            if !provider_type.is_empty() {
                update.provider_name = Some(provider_type);
            }
        }
    }
    crate::kit::service_snapshot::publish_session_fields(update);
    if switch_active {
        *MODEL_HIGHLIGHT_UNTIL.state().write() =
            Some(peri_time::monotonic_now() + Duration::from_secs(2));
    }
}

/// 解析档位实际模型名：Profile.model > ProviderModels 映射 > alias label。
fn resolve_model_name_for_alias(app_config: &crate::config::AppConfig, alias: &str) -> String {
    let profile = app_config.profiles.get(alias);
    let provider = profile.and_then(|pf| {
        if pf.provider.is_empty() {
            app_config.providers.first()
        } else {
            app_config.providers.iter().find(|p| p.id == pf.provider)
        }
    });
    profile
        .and_then(|pf| pf.model.clone().filter(|m| !m.is_empty()))
        .or_else(|| {
            provider
                .and_then(|p| p.models.get_model(alias))
                .map(str::to_string)
        })
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| alias.to_string())
}

fn notify_save_result(result: Result<(), anyhow::Error>) {
    match result {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: peri_time::monotonic_now() + Duration::from_secs(1),
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
                until: peri_time::monotonic_now() + Duration::from_secs(2),
            });
        }
    }
}
