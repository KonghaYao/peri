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
