//! `config_service` 的落盘边界与提示契约回归。
//!
//! 只经注入 `save` 的入口观察两条路径：成功写 `config-saved`（1s）并返回 `true`，
//! 失败写 `config-save-failed`（2s，携带原因）并返回 `false`——调用方据此不提交
//! 内存句柄与投影。面板侧的同类回归见
//! `kit::panels::config_test::base_row_save_failure_keeps_persisted_value_and_reports`
//! 与 `kit::panels::model::commit_test::model_commit_save_failure_keeps_memory_and_pushes_nothing`。

use super::persist_candidate_with;
use crate::config::PeriConfig;
use crate::i18n;
use crate::kit::atoms::{NOTIFICATION, Notification};
use fluent_bundle::FluentValue;
use serial_test::serial;
use std::time::Duration;

/// 提示的剩余时长下限：`until` 由被测代码与断言各取一次单调时钟，取 900/1900ms
/// 容忍这段极短的执行时间，不引入 sleep。
const SAVED_TTL_FLOOR: Duration = Duration::from_millis(900);
const SAVE_FAILED_TTL_FLOOR: Duration = Duration::from_millis(1900);

/// 全局提示的保存/恢复——断言失败也不污染其它用例。
struct NotificationGuard(Option<Notification>);

impl NotificationGuard {
    fn install() -> Self {
        Self(std::mem::take(&mut *NOTIFICATION.state().write()))
    }

    /// 最近一次提示的文案与剩余时长。
    fn latest(&self) -> (String, Duration) {
        let binding = NOTIFICATION.state();
        let state = binding.read();
        let note = state.as_ref().expect("落盘结果必须给出可见提示");
        (
            note.message.clone(),
            note.until - peri_time::monotonic_now(),
        )
    }
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        *NOTIFICATION.state().write() = self.0.take();
    }
}

/// 候选配置（与 `PeriConfig::default()` 可区分，便于断言落盘入口收到的内容）。
fn candidate() -> PeriConfig {
    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "opus".to_string();
    cfg
}

/// 成功：返回 `true`、落盘入口收到候选、提示为 `config-saved` 且时长 1s。
#[test]
#[serial]
fn success_reports_saved_with_one_second_ttl() {
    let guard = NotificationGuard::install();
    let candidate = candidate();
    let mut saved: Option<PeriConfig> = None;
    let mut save_calls = 0usize;

    let persisted = persist_candidate_with(&candidate, |cfg| {
        save_calls += 1;
        saved = Some(cfg.clone());
        Ok(())
    });

    assert!(persisted, "落盘成功必须返回 true（调用方据此提交）");
    assert_eq!(save_calls, 1, "候选只落盘一次");
    assert_eq!(saved, Some(candidate), "落盘入口必须收到调用方持有的候选");
    let (message, ttl) = guard.latest();
    assert_eq!(message, i18n::tr("config-saved"));
    assert!(
        ttl > SAVED_TTL_FLOOR && ttl <= Duration::from_secs(1),
        "成功提示时长应为 1s，实际 {ttl:?}"
    );
}

/// 失败：返回 `false`、提示为 `config-save-failed` 且携带原因、时长 2s。
#[test]
#[serial]
fn failure_reports_reason_with_two_second_ttl() {
    let guard = NotificationGuard::install();
    let candidate = candidate();
    let mut save_calls = 0usize;

    let persisted = persist_candidate_with(&candidate, |_| {
        save_calls += 1;
        Err(anyhow::anyhow!("configuration revision conflict"))
    });

    assert!(!persisted, "落盘失败必须返回 false（调用方据此不提交）");
    assert_eq!(save_calls, 1);
    let (message, ttl) = guard.latest();
    assert_eq!(
        message,
        i18n::tr_args(
            "config-save-failed",
            &[(
                "error".to_string(),
                FluentValue::from("configuration revision conflict"),
            )],
        )
    );
    assert!(
        message.contains("configuration revision conflict"),
        "失败提示必须携带原因：{message}"
    );
    assert!(
        ttl > SAVE_FAILED_TTL_FLOOR && ttl <= Duration::from_secs(2),
        "失败提示时长应为 2s，实际 {ttl:?}"
    );
}
