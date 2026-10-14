//! canonical reminder 的受众投递矩阵（H8）：模型投影与客户端快照各自按目标
//! audience 过滤，持久化 canonical reminder 不受影响。

use super::*;

/// H8：模型投影与客户端快照各自按目标 audience 过滤，且持久化 canonical
/// reminder 不受影响。
fn reminder_with(
    kind: &str,
    delivery: peri_acp_types::system_reminder::ReminderDelivery,
    audiences: Vec<peri_acp_types::system_reminder::ReminderAudience>,
) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    use peri_acp_types::system_reminder::{
        ReminderAudiences, ReminderCategory, ReminderSeverity, ReminderSource, SystemReminder,
        TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Guidance,
            source: ReminderSource("audience_test".into()),
            kind: kind.into(),
            severity: ReminderSeverity::Info,
            delivery,
            audiences: ReminderAudiences(audiences),
            body: format!("body of {kind}"),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap()
}

#[test]
fn audience_matrix_splits_model_projection_from_client_snapshot() {
    use peri_acp_types::system_reminder::{ReminderAudience, ReminderDelivery};

    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::human("user turn"));
    let client_only_id = transcript.append_system_reminder(reminder_with(
        "client_only",
        ReminderDelivery::Required,
        vec![ReminderAudience::Tui],
    ));
    let model_only_id = transcript.append_system_reminder(reminder_with(
        "model_only",
        ReminderDelivery::Configurable,
        vec![ReminderAudience::Model],
    ));
    let both_id = transcript.append_system_reminder(reminder_with(
        "both",
        ReminderDelivery::Configurable,
        vec![ReminderAudience::Model, ReminderAudience::Tui],
    ));

    let model = transcript.visible_model_messages().unwrap();
    let model_ids: Vec<_> = model.iter().map(BaseMessage::id).collect();
    assert!(
        !model_ids.contains(&client_only_id),
        "Tui-only 提醒不得进入模型推理"
    );
    assert!(model_ids.contains(&model_only_id));
    assert!(model_ids.contains(&both_id));

    let snapshot = transcript.visible_snapshot();
    let snapshot_ids: Vec<_> = snapshot.iter().map(BaseMessage::id).collect();
    assert!(
        !snapshot_ids.contains(&model_only_id),
        "Model-only 内容不得出现在客户端 wire 上"
    );
    assert!(snapshot_ids.contains(&client_only_id));
    assert!(snapshot_ids.contains(&both_id));

    for id in [client_only_id, model_only_id, both_id] {
        assert!(
            matches!(transcript.get(id), Some(TranscriptEntry::Reminder { .. })),
            "过滤只发生在出口，持久化保留 canonical reminder"
        );
    }
}

#[test]
fn diagnostic_only_reminders_reach_neither_model_nor_client() {
    use peri_acp_types::system_reminder::{ReminderAudience, ReminderDelivery};

    let mut transcript = MessageTranscript::new();
    let id = transcript.append_system_reminder(reminder_with(
        "diagnostic",
        ReminderDelivery::DiagnosticOnly,
        vec![ReminderAudience::Model, ReminderAudience::Tui],
    ));

    let model_ids: Vec<_> = transcript
        .visible_model_messages()
        .unwrap()
        .iter()
        .map(BaseMessage::id)
        .collect();
    let snapshot_ids: Vec<_> = transcript
        .visible_snapshot()
        .iter()
        .map(BaseMessage::id)
        .collect();
    assert!(!model_ids.contains(&id));
    assert!(!snapshot_ids.contains(&id));
}
