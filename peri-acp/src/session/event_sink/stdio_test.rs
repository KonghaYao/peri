//! stdio 出口的 reminder 承载决策（H8 过滤 + M13 caps 分流）。

use super::{reminder_carriage, StdioReminderCarriage};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::PeriCaps;

fn reminder(
    kind: &str,
    delivery: ReminderDelivery,
    audiences: Vec<ReminderAudience>,
) -> SystemReminder {
    SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Guidance,
        source: ReminderSource("stdio_test".into()),
        kind: kind.into(),
        severity: ReminderSeverity::Info,
        delivery,
        audiences: ReminderAudiences(audiences),
        body: "body".into(),
        summary: None,
        metadata: serde_json::json!({}),
    }
}

#[test]
fn model_only_and_diagnostic_only_reminders_are_filtered_before_carriage() {
    let structured = PeriCaps {
        system_reminder: true,
        ..PeriCaps::default()
    };
    for caps in [structured, PeriCaps::default()] {
        for filtered in [
            reminder(
                "recall",
                ReminderDelivery::Configurable,
                vec![ReminderAudience::Model],
            ),
            reminder(
                "diagnostic",
                ReminderDelivery::DiagnosticOnly,
                vec![ReminderAudience::Model, ReminderAudience::Tui],
            ),
        ] {
            assert_eq!(
                reminder_carriage(&caps, &filtered),
                StdioReminderCarriage::FilteredOut,
                "未声明客户端受众的内容不得下发（Model-only wire 零泄漏）"
            );
        }
    }
}

#[test]
fn client_audience_reminders_follow_session_caps() {
    let client_only = reminder(
        "client_notice",
        ReminderDelivery::Required,
        vec![ReminderAudience::Tui],
    );
    assert_eq!(
        reminder_carriage(
            &PeriCaps {
                system_reminder: true,
                ..PeriCaps::default()
            },
            &client_only
        ),
        StdioReminderCarriage::Structured
    );
    assert_eq!(
        reminder_carriage(&PeriCaps::default(), &client_only),
        StdioReminderCarriage::NotCarried,
        "未协商结构化提醒的客户端必须显式不承载，而不是继承默认 no-op"
    );
}

/// stdio 结构化 reminder 的 wire 契约（M13「stdio 出口」验收）。
///
/// 方法名 `peri/systemReminder` 与 params 字段名（`sessionId` / `reminder` /
/// `replay`）是对外 SDK 客户端依赖的公开面，目前仅由
/// `#[notification(method = ...)]` 属性静态承载。属性被改写、方法名笔误或字段
/// 重命名都会静默破坏客户端兼容，因此必须由断言锁死，不能只靠属性文本。
#[test]
fn structured_reminder_notification_pins_the_peri_wire_contract() {
    use agent_client_protocol::JsonRpcMessage;

    type Notification = super::PeriSystemReminderNotification;
    let notification = Notification {
        session_id: agent_client_protocol::schema::v1::SessionId::new("s-wire"),
        reminder: reminder(
            "wire_pinned",
            ReminderDelivery::Required,
            vec![ReminderAudience::Tui],
        ),
        replay: true,
    };

    assert_eq!(notification.method(), "peri/systemReminder");
    assert!(Notification::matches_method("peri/systemReminder"));
    assert!(
        !Notification::matches_method("session/update"),
        "标准更新方法名不得被该扩展通知吞并"
    );

    let untyped = notification.to_untyped_message().unwrap();
    assert_eq!(
        untyped.method(),
        "peri/systemReminder",
        "wire 上的方法名必须与 caps 协商的 peri.systemReminder 对应"
    );
    let params = untyped.params();
    assert_eq!(params["sessionId"], "s-wire");
    assert_eq!(params["replay"], true);
    assert_eq!(params["reminder"]["kind"], "wire_pinned");
    assert_eq!(params["reminder"]["source"], "stdio_test");
    assert!(
        params["reminder"]["body"].is_string(),
        "canonical DTO 正文随结构化通知承载"
    );
    assert!(
        params["reminder"]["audiences"].is_array(),
        "受众声明必须随 DTO 下发，客户端据此决定是否展示"
    );
}
