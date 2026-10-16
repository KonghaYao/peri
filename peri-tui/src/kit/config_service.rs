//! 配置编辑面板共用的落盘边界与用户可见提示（单一权威）。
//!
//! Config / Model / Login / Theme 四个配置编辑面板遵循同一条规则：候选配置先落盘，
//! 成功才由调用方提交内存句柄、投影与 ACP 推送；失败给出可见提示并保持已持久化的值。
//! 规则、文案与时长只在此处维护一份——各面板的候选构造方式与成功后的发布动作语义
//! 不同，仍归各面板所有。

use crate::config::{PeriConfig, save_effective};
use crate::i18n;
use crate::kit::atoms::{NOTIFICATION, Notification};
use fluent_bundle::FluentValue;
use std::time::Duration;

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

fn notify(message: String, ttl: Duration) {
    *NOTIFICATION.state().write() = Some(Notification {
        message,
        until: peri_time::monotonic_now() + ttl,
    });
}

#[cfg(test)]
#[path = "config_service_test.rs"]
mod tests;
