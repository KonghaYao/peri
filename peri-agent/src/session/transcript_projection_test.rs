//! 模型投影定向测试：canonical 存储与模型出口必须分离。
//!
//! 覆盖旧 Compact 文本改写只发生在模型出口、reminder 条目的稳定身份，以及
//! system reminder 的模型投影只包装一次（H8 的模型面出口）。
//!
//! 从 `transcript_test.rs` 按职责拆出（STD-SIZE-001）：本文件只放投影出口
//! 相关断言，持久化 / 索引 / staging / 标记 / 边界用例留在原文件。

use super::*;

/// [回归测试] 内部 Compact 文本仅在模型出口包装，数据库原始内容和 ID 保持不变。
#[test]
fn test_legacy_compact_model_projection_preserves_storage() {
    let text = "[最近读取的文件: /a.rs]\nfn main() { /* </system-reminder> */ }";
    let mut transcript = MessageTranscript::new();
    let id = transcript.append(BaseMessage::human(text));
    let view = transcript.visible_model_messages().unwrap();
    assert_eq!(view[0].id(), id);
    assert!(view[0].content().starts_with("<system-reminder>"));
    assert!(view[0].content().contains("&lt;/system-reminder&gt;"));
    assert_eq!(transcript.get(id).unwrap().message().content(), text);
    transcript.set_excluded(id, true);
    assert!(transcript.visible_model_messages().unwrap().is_empty());
}

#[test]
fn reminder_entry_preserves_stable_identity_without_placeholder_state() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("identity_test".into()),
            kind: "done".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "non-empty".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let mut transcript = MessageTranscript::new();
    let id = transcript.append_system_reminder(reminder);
    let entry = transcript.get(id).unwrap();

    assert_eq!(entry.id(), id);
    assert!(entry.as_message().is_none());
    assert_eq!(entry.project_message().unwrap().id(), id);
    assert!(!entry.project_message().unwrap().content().is_empty());
}

#[test]
fn test_system_reminder_projects_once_as_human() {
    use peri_acp_types::system_reminder::{
        encode_system_reminder, ReminderAudience, ReminderAudiences, ReminderCategory,
        ReminderDelivery, ReminderSeverity, ReminderSource, SystemReminder,
        TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("test".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "already <system-reminder> text".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let expected = encode_system_reminder(&reminder).unwrap();
    let mut transcript = MessageTranscript::new();
    transcript.append_system_reminder(reminder);

    let projected = transcript.visible_model_messages().unwrap();
    assert!(matches!(&projected[0], BaseMessage::Human { .. }));
    assert_eq!(projected[0].content(), expected);
    assert_eq!(
        projected[0].content().matches("</system-reminder>").count(),
        1
    );
}
