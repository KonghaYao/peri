//! `_meta.peri.recovery` 解码与通知投影的纯客户端行为。

use super::NOTIFICATION;
use super::Notification;
use super::RecoveryView;

use peri_acp_types::recovery::RecoveryViewStatus;

fn failure_view(blocked: bool) -> RecoveryView {
    RecoveryView {
        status: RecoveryViewStatus::Failed,
        root_thread_id: Some("session-a".to_owned()),
        entered_thread_id: "session-a".to_owned(),
        recovered: Vec::new(),
        reused: Vec::new(),
        skipped: Vec::new(),
        failures: Vec::new(),
        error: Some(peri_acp_types::recovery::RecoveryFailureView {
            thread_id: "session-a".to_owned(),
            kind: peri_acp_types::recovery::RecoveryErrorKind::Corrupt,
            detail: "missing parent".to_owned(),
            execution_blocked: blocked,
        }),
        execution_blocked: blocked,
    }
}

#[test]
fn decode_reads_the_flat_recovery_key() {
    let value = serde_json::json!({"_meta": {"peri.recovery": failure_view(true)}});
    let view = super::decode(&value).expect("恢复字段必须可解码");
    assert_eq!(view.status, RecoveryViewStatus::Failed);
    assert!(view.execution_blocked);
    assert_eq!(view.error.expect("decode reads the flat recovery key: the fixture establishes view.error before this assertion").detail, "missing parent");
}

#[test]
fn decode_ignores_malformed_or_missing_meta() {
    assert!(super::decode(&serde_json::json!({})).is_none());
    assert!(super::decode(&serde_json::json!({"_meta": {"peri.recovery": "broken"}})).is_none());
    assert!(
        super::decode(&serde_json::json!({"_meta": {"peri.recovery": {"status": "nope"}}}))
            .is_none()
    );
}

struct NotificationGuard(Option<String>);

impl NotificationGuard {
    fn capture() -> Self {
        let state = NOTIFICATION.state();
        let notice = state.read();
        Self(notice.as_ref().map(|notice| notice.message.clone()))
    }
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        NOTIFICATION.set(self.0.take().map(|message| Notification {
            message,
            until: peri_time::monotonic_now() + std::time::Duration::from_secs(1),
        }));
    }
}

#[test]
#[serial_test::serial]
fn failed_blocked_view_sets_notification_with_reason() {
    let _notification = NotificationGuard::capture();
    super::present("session-a", &failure_view(true));
    let state = NOTIFICATION.state();
    let notice = state.read();
    let notice = notice.as_ref().expect("失败恢复必须有通知");
    assert!(notice.message.contains("corrupt"), "{}", notice.message);
    assert!(
        notice.message.contains("missing parent"),
        "{}",
        notice.message
    );
}

fn partial_view(count: usize) -> RecoveryView {
    RecoveryView {
        status: RecoveryViewStatus::Partial,
        root_thread_id: Some("session-a".to_owned()),
        entered_thread_id: "session-a".to_owned(),
        recovered: Vec::new(),
        reused: Vec::new(),
        skipped: Vec::new(),
        failures: (0..count)
            .map(|index| peri_acp_types::recovery::RecoveryFailureView {
                thread_id: format!("child-{index}"),
                kind: peri_acp_types::recovery::RecoveryErrorKind::Unavailable,
                detail: "child completion target is unavailable; stop and explicitly resume the child to establish a delivery route".to_owned(),
                execution_blocked: false,
            })
            .collect(),
        error: None,
        execution_blocked: false,
    }
}

#[test]
#[serial_test::serial]
fn partial_notice_asks_for_explicit_resume_and_counts_child_threads() {
    let _notification = NotificationGuard::capture();
    crate::i18n::init(Some("en"));
    super::present("session-a", &partial_view(121));
    let state = NOTIFICATION.state();
    let notice = state.read();
    let message = notice.as_ref().expect("部分恢复必须有通知").message.clone();
    drop(notice);
    assert!(message.contains("121"), "{message}");
    assert!(message.contains("explicit resume"), "{message}");
    assert!(!message.contains("could not be recovered"), "{message}");

    crate::i18n::init(Some("zh-CN"));
    super::present("session-a", &partial_view(2));
    let state = NOTIFICATION.state();
    let notice = state.read();
    let message = notice.as_ref().expect("部分恢复必须有通知").message.clone();
    drop(notice);
    assert!(
        message.contains("2 个相关子线程需要显式 resume（缺少投递目标或恢复失败）"),
        "{message}"
    );
    crate::i18n::init(None);
}

#[test]
#[serial_test::serial]
fn ok_view_produces_no_notification() {
    let _notification = NotificationGuard::capture();
    let view = RecoveryView {
        status: RecoveryViewStatus::Ok,
        ..failure_view(false)
    };
    super::present("session-a", &view);
    assert!(NOTIFICATION.state().read().is_none());
}
