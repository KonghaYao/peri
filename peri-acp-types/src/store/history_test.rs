//! 纯历史变换的行为测试：fork 重映射、投影 flag、compaction 批次与 rewind 边界。

use super::*;
use crate::projection::{ProjectionAction, ProjectionActionEntry, ProjectionTarget};
use crate::session_resources::RewindBoundary;
use crate::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

/// 确定性 ID 分配器：同一输入在测试中必然得到同一结果（本模块不产生随机性）。
fn deterministic_ids() -> impl FnMut() -> MessageId {
    let mut counter = 0u128;
    move || {
        counter += 1;
        MessageId::from(uuid::Uuid::from_u128(counter))
    }
}

fn reminder_payload() -> PersistedPayload {
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Security,
        source: ReminderSource("history_test".into()),
        kind: "notice".into(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Model]),
        body: "body".into(),
        summary: None,
        metadata: serde_json::json!({}),
    };
    PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: TrustedSystemReminderFactory::for_producer()
            .construct(reminder)
            .unwrap(),
    }
}

fn directive_referencing(id: MessageId) -> MessageProjectionDirective {
    MessageProjectionDirective {
        policy_version: 1,
        entries: vec![ProjectionActionEntry {
            message_id: id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactText { max_chars: 10 },
        }],
    }
}

#[test]
fn fork_remap_allocates_new_ids_and_keeps_payload_discriminants() {
    let message = PersistedPayload::Message(BaseMessage::human("hello"));
    let source_payloads = vec![message.clone(), reminder_payload()];
    let forked = remap_fork_history(&source_payloads, &HashMap::new(), deterministic_ids());

    assert_eq!(forked.payloads.len(), 2);
    assert!(matches!(forked.payloads[0], PersistedPayload::Message(_)));
    assert!(matches!(
        forked.payloads[1],
        PersistedPayload::SystemReminder { .. }
    ));
    // 新 ID 必须与 source 不同：SQLite 的 message_id 是全库主键，沿用会让 fork 丢行。
    for (source, copied) in source_payloads.iter().zip(&forked.payloads) {
        assert_ne!(source.id(), copied.id());
    }
}

#[test]
fn fork_remap_rewrites_projection_references_and_drops_foreign_flags() {
    let source_payloads = vec![
        PersistedPayload::Message(BaseMessage::human("a")),
        PersistedPayload::Message(BaseMessage::ai("b")),
    ];
    let mut source_flags = HashMap::new();
    source_flags.insert(
        source_payloads[0].id(),
        MessageFlags {
            truncated: false,
            excluded: true,
            projection: Some(directive_referencing(source_payloads[0].id())),
        },
    );
    // 不在 payload 里的 flags 属于 source 的其他消息：不能带进 fork 目标。
    source_flags.insert(
        MessageId::new(),
        MessageFlags {
            truncated: true,
            excluded: false,
            projection: None,
        },
    );
    let payloads_before = payload_ids(&source_payloads);
    let flags_before = source_flags.clone();

    let forked = remap_fork_history(&source_payloads, &source_flags, deterministic_ids());

    assert_eq!(forked.flags.len(), 1);
    let forked_id = forked.payloads[0].id();
    let flags = forked.flags.get(&forked_id).expect("forked flags");
    assert!(flags.excluded, "既有标记必须保留");
    assert!(!flags.truncated);
    let directive = flags.projection.as_ref().expect("projection preserved");
    assert_eq!(directive.entries.len(), 1);
    assert_eq!(directive.entries[0].message_id, forked_id);
    // source 只读：变换不得修改输入。
    assert_eq!(payload_ids(&source_payloads), payloads_before);
    assert_eq!(source_flags, flags_before);
}

#[test]
fn fork_remap_without_source_flags_yields_empty_flags() {
    let source_payloads = vec![PersistedPayload::Message(BaseMessage::human("a"))];

    let forked = remap_fork_history(&source_payloads, &HashMap::new(), deterministic_ids());

    assert!(forked.flags.is_empty());
    assert_eq!(forked.payloads.len(), 1);
}

#[test]
fn projection_flag_rule_marks_truncated_and_keeps_excluded() {
    let id = MessageId::new();
    let existing = MessageFlags {
        truncated: false,
        excluded: true,
        projection: None,
    };

    let flags = flags_with_projection(&existing, directive_referencing(id));

    assert!(flags.truncated, "投影与 truncated 是同一规则");
    assert!(flags.excluded, "投影不解除排除标记");
    assert!(flags.projection.is_some());
    assert!(!existing.truncated, "输入不被修改");
}

#[test]
fn flag_updates_remove_defaults_and_apply_in_order() {
    let first = MessageId::new();
    let second = MessageId::new();
    let mut flags = HashMap::new();
    flags.insert(
        first,
        MessageFlags {
            truncated: true,
            excluded: false,
            projection: None,
        },
    );

    apply_flag_updates(
        &mut flags,
        &[
            (first, MessageFlags::default()),
            (
                second,
                MessageFlags {
                    truncated: false,
                    excluded: true,
                    projection: None,
                },
            ),
        ],
    );
    assert!(!flags.contains_key(&first), "默认 flags 表示无标记");

    apply_flag_updates(
        &mut flags,
        &[
            (
                second,
                MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: None,
                },
            ),
            (second, MessageFlags::default()),
        ],
    );
    assert!(
        !flags.contains_key(&second),
        "同一 id 在批次内以最后一次为准"
    );
}

#[test]
fn distinct_ids_reject_batch_duplicates_and_known_ids() {
    let known = PersistedPayload::Message(BaseMessage::human("known"));
    let known_id = known.id();
    let fresh = PersistedPayload::Message(BaseMessage::human("fresh"));

    assert!(ensure_distinct_ids(std::slice::from_ref(&fresh), |id| id == known_id).is_ok());

    let error = ensure_distinct_ids(&[fresh.clone(), fresh.clone()], |_| false).unwrap_err();
    assert_eq!(
        error,
        HistoryError::DuplicateId { id: fresh.id() },
        "批次内重复必须失败，不能静默忽略"
    );

    let error = ensure_distinct_ids(std::slice::from_ref(&known), |id| id == known_id).unwrap_err();
    assert_eq!(error, HistoryError::DuplicateId { id: known_id });
}

fn payload_ids(payloads: &[PersistedPayload]) -> Vec<MessageId> {
    payloads.iter().map(PersistedPayload::id).collect()
}

#[test]
fn rewind_keeps_or_removes_the_target_itself() {
    let payloads = vec![
        PersistedPayload::Message(BaseMessage::human("a")),
        PersistedPayload::Message(BaseMessage::ai("b")),
        PersistedPayload::Message(BaseMessage::human("c")),
    ];
    let target = payloads[1].id();

    let keep = apply_rewind(&payloads, RewindBoundary::KeepThrough(target)).unwrap();
    assert_eq!(payload_ids(&keep.kept), payload_ids(&payloads[..2]));
    assert_eq!(keep.removed, vec![payloads[2].id()]);

    let remove = apply_rewind(&payloads, RewindBoundary::RemoveFrom(target)).unwrap();
    assert_eq!(payload_ids(&remove.kept), payload_ids(&payloads[..1]));
    assert_eq!(remove.removed, vec![target, payloads[2].id()]);
}

#[test]
fn rewind_missing_target_fails_before_touching_history() {
    let payloads = vec![PersistedPayload::Message(BaseMessage::human("a"))];
    let missing = MessageId::new();
    let before = payload_ids(&payloads);

    let error = apply_rewind(&payloads, RewindBoundary::RemoveFrom(missing)).unwrap_err();

    assert_eq!(error, HistoryError::TargetMissing { id: missing });
    assert_eq!(payload_ids(&payloads), before, "失败不产生部分变换");
    assert_eq!(
        rewind_keep_len(
            &payload_ids(&payloads),
            RewindBoundary::KeepThrough(missing)
        )
        .unwrap_err(),
        HistoryError::TargetMissing { id: missing }
    );
}

#[test]
fn compaction_change_applies_flags_and_appends_messages() {
    let mut payloads = vec![PersistedPayload::Message(BaseMessage::human("a"))];
    let existing_id = payloads[0].id();
    let mut flags = HashMap::new();
    let appended = BaseMessage::ai("summary");
    let change = CompactionChange {
        flag_updates: vec![(
            existing_id,
            MessageFlags {
                truncated: false,
                excluded: true,
                projection: None,
            },
        )],
        appended_messages: vec![appended.clone()],
    };

    apply_compaction_change(&mut payloads, &mut flags, &change).unwrap();

    assert_eq!(payload_ids(&payloads), vec![existing_id, appended.id()]);
    assert!(flags[&existing_id].excluded);
}

#[test]
fn compaction_change_rejects_duplicate_id_without_partial_apply() {
    let existing = PersistedPayload::Message(BaseMessage::human("a"));
    let mut payloads = vec![existing.clone()];
    let mut flags = HashMap::new();
    let before = payload_ids(&payloads);
    let change = CompactionChange {
        flag_updates: vec![(
            existing.id(),
            MessageFlags {
                truncated: false,
                excluded: true,
                projection: None,
            },
        )],
        appended_messages: vec![BaseMessage::ai("summary").with_message_id(existing.id())],
    };

    let error = apply_compaction_change(&mut payloads, &mut flags, &change).unwrap_err();

    assert_eq!(error, HistoryError::DuplicateId { id: existing.id() });
    assert_eq!(payload_ids(&payloads), before, "整体生效或不生效");
    assert!(flags.is_empty(), "失败时 flags 也不得被改");
}

#[test]
fn appended_payloads_preserve_message_identity() {
    let message = BaseMessage::ai("summary");

    let payloads = appended_payloads(std::slice::from_ref(&message));

    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].id(), message.id());
}
