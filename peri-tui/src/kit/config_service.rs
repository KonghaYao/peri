//! 配置编辑面板共用的落盘边界与用户可见提示（单一权威）。
//!
//! Config / Model / Login / Theme 四个配置编辑面板遵循同一条规则：候选配置先落盘，
//! 成功才由调用方提交内存句柄、投影与 ACP 推送；失败给出可见提示并保持已持久化的值。
//! 规则、文案与时长只在此处维护一份——各面板的候选构造方式与成功后的发布动作语义
//! 不同，仍归各面板所有。
//!
//! 两条保存入口：
//! - [`persist_candidate`]：即时编辑（切换、单个字段），保存时取最新 revision；
//! - [`persist_draft`]：长草稿（Login / Model 内联 editor），调用方捕获 expected
//!   revision，成功返回 authority 接纳后的快照，失败区分 revision 冲突与普通失败。

use crate::config::{
    ConfigurationRevision, PeriConfig, SaveEffectiveError, save_effective, save_effective_at,
};
use crate::i18n;
use crate::kit::atoms::{NOTIFICATION, Notification};
use fluent_bundle::FluentValue;
use std::time::Duration;

/// 长草稿保存失败：调用方据此决定提示、焦点与是否允许重新载入。
#[derive(Debug)]
pub(crate) enum DraftSaveError {
    /// expected revision 已过期（外部写入 / 另一个面板先保存）。
    Conflict,
    /// 磁盘、权限、authority 或校验失败。
    Failed(anyhow::Error),
}

/// 落盘候选配置，并给出统一的用户可见提示。
///
/// 成功返回 `true` 并把 `config-saved`（1s）写入 `NOTIFICATION`；
/// 失败返回 `false`：记录 `tracing::warn`，把 `config-save-failed`（带 error 参数，
/// 2s）写入 `NOTIFICATION`。
///
/// 契约：返回 `false` 时调用方**不得**提交内存句柄、投影或推送 ACP——
/// 用户看到失败提示时，可见状态必须仍是已持久化的值。
pub(crate) fn persist_candidate(candidate: &PeriConfig) -> bool {
    persist_candidate_with(candidate, save_effective)
}

/// 注入 `save` 的版本：测试用它观察「失败不发布」与提示，而不落真实磁盘。
pub(crate) fn persist_candidate_with(
    candidate: &PeriConfig,
    save: impl FnOnce(&PeriConfig) -> anyhow::Result<()>,
) -> bool {
    match save(candidate) {
        Ok(()) => {
            notify_saved();
            true
        }
        Err(error) => {
            notify_save_failed(&error);
            false
        }
    }
}

/// 长草稿保存：显式 expected revision + accepted snapshot。
///
/// 成功返回 authority 接纳后的配置，调用方只能发布该值；`Conflict` 单独提示，
/// 普通失败沿用 `config-save-failed`。
pub(crate) fn persist_draft(
    candidate: &PeriConfig,
    expected_revision: ConfigurationRevision,
) -> Result<PeriConfig, DraftSaveError> {
    persist_draft_with(candidate, expected_revision, |revision, cfg| {
        save_effective_at(revision, cfg).map_err(DraftSaveError::from_save_error)
    })
}

impl DraftSaveError {
    fn from_save_error(error: SaveEffectiveError) -> Self {
        if error.is_conflict() {
            Self::Conflict
        } else {
            Self::Failed(error.into())
        }
    }
}

/// 注入保存入口的版本：测试可断言 expected revision 原样传递、accepted 返回与提示，
/// 不落真实磁盘。
pub(crate) fn persist_draft_with(
    candidate: &PeriConfig,
    expected_revision: ConfigurationRevision,
    save: impl FnOnce(ConfigurationRevision, &PeriConfig) -> Result<PeriConfig, DraftSaveError>,
) -> Result<PeriConfig, DraftSaveError> {
    match save(expected_revision, candidate) {
        Ok(accepted) => {
            notify_saved();
            Ok(accepted)
        }
        Err(DraftSaveError::Conflict) => {
            tracing::warn!("配置草稿保存失败：revision 冲突");
            notify(i18n::tr("config-draft-conflict"), Duration::from_secs(3));
            Err(DraftSaveError::Conflict)
        }
        Err(DraftSaveError::Failed(error)) => {
            notify_save_failed(&error);
            Err(DraftSaveError::Failed(error))
        }
    }
}

/// 保存成功提示。
fn notify_saved() {
    notify(i18n::tr("config-saved"), Duration::from_secs(1));
}

/// 保存失败提示：携带失败原因（面板既有的失败呈现机制）。
///
/// 面板自行判定的失败（如行未接线、活动档位缺失）也经此提示，文案保持单一来源。
pub(crate) fn notify_save_failed(error: &anyhow::Error) {
    tracing::warn!(error = %error, "配置保存失败");
    notify(
        i18n::tr_args(
            "config-save-failed",
            &[(
                "error".to_string(),
                FluentValue::from(error.to_string().as_str()),
            )],
        ),
        Duration::from_secs(2),
    );
}

/// 配置已落盘但 ACP 运行端同步失败。
///
/// 这是保存成功之后的独立结果：不得提示「保存失败」，也不回滚已写入的配置。
pub(crate) fn notify_runtime_sync_failed(error: &anyhow::Error) {
    tracing::warn!(error = %error, "配置已保存，ACP 运行端同步失败");
    notify(
        i18n::tr_args(
            "config-runtime-sync-failed",
            &[(
                "error".to_string(),
                FluentValue::from(error.to_string().as_str()),
            )],
        ),
        Duration::from_secs(3),
    );
}

/// 重新从配置 authority 读取并发布最新快照（不写文件）。
///
/// 与保存不同，这是用户显式发起的“重新载入”：成功后 `PERI_CONFIG_HANDLE` 与
/// authority 快照一致，调用方可用 [`crate::config::configuration_revision`] 取到
/// 最新 revision 重新基线化草稿；失败时不改变任何已发布状态。
///
/// Provider 投影（`PROVIDER_LIST`）由需要它的 Login 面板在调用后自行刷新，
/// 避免把面板投影规则复制进共用保存边界。
pub(crate) fn reload_configuration() -> anyhow::Result<crate::config::PeriConfig> {
    let accepted = crate::config::reload_effective()?;
    if let Some(handle) = crate::kit::atoms::PERI_CONFIG_HANDLE.get() {
        *handle.write() = accepted.clone();
    }
    Ok(accepted)
}

/// 显式重载成功提示。
pub(crate) fn notify_reloaded() {
    notify(i18n::tr("config-reloaded"), Duration::from_secs(2));
}

/// 显式重载失败提示：与保存失败区分，避免暗示磁盘内容已被改写。
pub(crate) fn notify_reload_failed(error: &anyhow::Error) {
    tracing::warn!(error = %error, "配置重新载入失败");
    notify(
        i18n::tr_args(
            "config-reload-failed",
            &[(
                "error".to_string(),
                FluentValue::from(error.to_string().as_str()),
            )],
        ),
        Duration::from_secs(3),
    );
}

fn notify(message: String, ttl: Duration) {
    *NOTIFICATION.state().write() = Some(Notification {
        message,
        until: peri_time::monotonic_now() + ttl,
    });
}

#[cfg(test)]
#[path = "config_service_test.rs"]
mod tests;
