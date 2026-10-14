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
        superseded_by: None,
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
fn fork_remap_rewrites_superseded_by_to_the_forked_summary() {
    let archived = PersistedPayload::Message(BaseMessage::human("archived"));
    let summary = PersistedPayload::Message(BaseMessage::ai("summary"));
    let source_payloads = vec![archived.clone(), summary.clone()];
    let mut source_flags = HashMap::new();
    source_flags.insert(
        archived.id(),
        MessageFlags {
            truncated: false,
            excluded: true,
            projection: Some(MessageProjectionDirective {
                policy_version: 1,
                entries: Vec::new(),
                superseded_by: Some(summary.id()),
            }),
        },
    );

    let forked = remap_fork_history(&source_payloads, &source_flags, deterministic_ids());

    let flags = forked
        .flags
        .get(&forked.payloads[0].id())
        .expect("forked flags");
    assert_eq!(
        flags.superseded_by(),
        Some(forked.payloads[1].id()),
        "取代关系必须指向 fork 目标里的摘要"
    );
    assert_ne!(
        flags.superseded_by(),
        Some(summary.id()),
        "源 id 不得残留在目标会话：源会话 rewind 掉摘要会让目标误判排除失去依据"
    );
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

// ─── rewind 有效视图重建：excluded 清零集合 ────────────────────────────────────

/// rewind 判定输入的 id：由序号派生（本模块不产生随机性）。
fn entry_id(index: u128) -> MessageId {
    MessageId::from(uuid::Uuid::from_u128(index))
}

/// rewind 判定输入：`(id, excluded, 是否 System)`。
fn flag_entry(index: u128, excluded: bool, is_system: bool) -> (MessageId, MessageFlags, bool) {
    (
        entry_id(index),
        MessageFlags {
            excluded,
            ..Default::default()
        },
        is_system,
    )
}

/// 带取代记录的排除条目：Full Compact 归档时写入 `superseded_by`，`entries` 为空
/// （取代关系是渲染之外的事实，见 `MessageProjectionDirective::superseded_by`）。
fn superseded_entry(index: u128, summary_index: u128) -> (MessageId, MessageFlags, bool) {
    let (id, mut flags, is_system) = flag_entry(index, true, false);
    flags.projection = Some(MessageProjectionDirective {
        policy_version: 1,
        entries: Vec::new(),
        superseded_by: Some(entry_id(summary_index)),
    });
    (id, flags, is_system)
}

/// 非 System 条的 id 序列：断言输出顺序与集合一起成立。
fn entry_ids(entries: &[(MessageId, MessageFlags, bool)]) -> Vec<MessageId> {
    entries.iter().map(|(id, _, _)| *id).collect()
}

#[test]
fn rewind_without_any_compaction_clears_nothing() {
    // (a) 没有任何 Full：前缀里不存在 excluded，清零集合为空。
    let entries = vec![flag_entry(1, false, false), flag_entry(2, false, false)];

    assert_eq!(rewind_clear_excluded_ids(&entries, 0), Vec::new());
    assert_eq!(rewind_clear_excluded_ids(&entries, 2), Vec::new());
}

#[test]
fn rewind_inside_the_excluded_block_restores_every_retained_body() {
    // (b) 一次 Full：A、B 被排除，摘要 S 不被排除。rewind 落在被排除区内时摘要被删掉，
    // 保留前缀的排除失去依据：A、B 都必须恢复有效（不能只删摘要）。
    let entries = vec![
        flag_entry(1, true, false),
        flag_entry(2, true, false),
        flag_entry(3, false, false),
    ];

    assert_eq!(
        rewind_clear_excluded_ids(&entries, 1),
        vec![entries[0].0],
        "只保留 A：A 失去依据，必须恢复"
    );
    assert_eq!(
        rewind_clear_excluded_ids(&entries, 2),
        vec![entries[0].0, entries[1].0],
        "保留 A、B：两条都失去依据"
    );
}

#[test]
fn rewind_after_the_block_keeps_the_prefix_excluded() {
    // (c) 一次 Full 且摘要存活：前缀 A、B 的排除仍由 S 支撑，不能被清零
    // （清零会同时恢复旧摘要和它覆盖的原文）。
    let entries = vec![
        flag_entry(1, true, false),
        flag_entry(2, true, false),
        flag_entry(3, false, false),
    ];

    assert_eq!(rewind_clear_excluded_ids(&entries, 3), Vec::new());
}

#[test]
fn rewind_after_surviving_summary_clears_only_entries_beyond_the_run() {
    // (c') 摘要之后仍有 excluded（违反不变量时也不得误清前缀）：只清 k 之后的。
    let entries = vec![
        flag_entry(1, true, false),
        flag_entry(2, true, false),
        flag_entry(3, false, false),
        flag_entry(4, true, false),
    ];

    assert_eq!(rewind_clear_excluded_ids(&entries, 4), vec![entries[3].0]);
}

#[test]
fn rewind_between_two_fulls_restores_what_the_removed_summary_covered() {
    // (d) 两次 Full：A、B 由第一次覆盖，S1、C 由第二次覆盖（S1 本身被第二次排除），
    // 第二次的摘要 S2 不被排除。rewind 落在两次之间 → S2 被删，保留前缀整段失去依据。
    let entries = vec![
        flag_entry(1, true, false),
        flag_entry(2, true, false),
        flag_entry(3, true, false),
        flag_entry(4, true, false),
        flag_entry(5, false, false),
    ];

    assert_eq!(
        rewind_clear_excluded_ids(&entries, 3),
        vec![entries[0].0, entries[1].0, entries[2].0]
    );
    assert_eq!(
        rewind_clear_excluded_ids(&entries, 4),
        vec![entries[0].0, entries[1].0, entries[2].0, entries[3].0]
    );
}

#[test]
fn rewind_after_the_second_full_changes_nothing() {
    // (e) 第二次 Full 的摘要仍然存活：整段排除仍有依据，一个都不清（不能全清）。
    let entries = vec![
        flag_entry(1, true, false),
        flag_entry(2, true, false),
        flag_entry(3, true, false),
        flag_entry(4, true, false),
        flag_entry(5, false, false),
    ];

    assert_eq!(rewind_clear_excluded_ids(&entries, 5), Vec::new());
    // 摘要之后新追加的条目（不被排除）不影响判定。
    let mut extended = entries.clone();
    extended.push(flag_entry(6, false, false));
    assert_eq!(rewind_clear_excluded_ids(&extended, 6), Vec::new());
}

#[test]
fn rewind_counts_the_excluded_run_ignoring_system_entries() {
    // (f) System 条目既不计入领导段长度，也不进清零集合（Full 从不排除它们）。
    let with_summary = vec![
        flag_entry(1, true, false),
        flag_entry(2, false, true),
        flag_entry(3, true, false),
        flag_entry(4, false, false),
    ];
    assert_eq!(
        rewind_clear_excluded_ids(&with_summary, 4),
        Vec::new(),
        "跳过 System 后领导段长度为 2，位置 2 的摘要存活"
    );

    let without_summary = vec![
        flag_entry(1, true, false),
        flag_entry(2, false, true),
        flag_entry(3, true, false),
    ];
    assert_eq!(
        rewind_clear_excluded_ids(&without_summary, 3),
        vec![without_summary[0].0, without_summary[2].0],
        "没有存活摘要时前缀的 non-System excluded 全部清零"
    );

    let excluded_system = vec![flag_entry(1, true, false), flag_entry(2, true, true)];
    assert_eq!(
        rewind_clear_excluded_ids(&excluded_system, 2),
        vec![excluded_system[0].0],
        "System 条目的 flags 不属于本函数的判定范围"
    );
}

#[test]
fn rewind_clear_ids_stay_within_the_retained_prefix() {
    // 清零集合永远取自保留前缀：被 rewind 删掉的条目不进集合（否则存储侧会去改已删除的行）。
    let entries = vec![
        flag_entry(1, true, false),
        flag_entry(2, true, false),
        flag_entry(3, true, false),
    ];

    let clear = rewind_clear_excluded_ids(&entries, 2);
    assert_eq!(clear, vec![entries[0].0, entries[1].0]);
    assert_eq!(
        entry_ids(&entries),
        vec![entries[0].0, entries[1].0, entries[2].0]
    );
    assert!(!clear.contains(&entries[2].0));
}

#[test]
fn rewind_between_two_fulls_keeps_the_batch_whose_summary_survives() {
    // (g) D8 批次边界：A、B 由 S1 取代，C、D 由 S2 取代。保留前缀止于 C、D 之后
    // （S2 被本次 rewind 删掉）⇒ 清零集合只含 C、D：A、B 的摘要 S1 仍在保留前缀里，
    // 排除依旧有依据，不能清零（清零会让旧摘要 S1 与被它覆盖的 A、B 同时有效）。
    let entries = vec![
        superseded_entry(1, 3),
        superseded_entry(2, 3),
        flag_entry(3, false, false),
        superseded_entry(4, 6),
        superseded_entry(5, 6),
        flag_entry(6, false, false),
    ];

    assert_eq!(
        rewind_clear_excluded_ids(&entries, 5),
        vec![entries[3].0, entries[4].0],
        "只清 C、D：它们的摘要 S2 已被 rewind 删掉"
    );
}

#[test]
fn rewind_dropping_the_summary_clears_the_batch_it_covered() {
    // (h) 保留前缀止于 S1 之前 ⇒ A、B 的取代依据随摘要一起消失，两条都必须恢复有效。
    let entries = vec![
        superseded_entry(1, 3),
        superseded_entry(2, 3),
        flag_entry(3, false, false),
        superseded_entry(4, 6),
        superseded_entry(5, 6),
        flag_entry(6, false, false),
    ];

    assert_eq!(
        rewind_clear_excluded_ids(&entries, 2),
        vec![entries[0].0, entries[1].0]
    );
    assert_eq!(
        rewind_clear_excluded_ids(&entries, 1),
        vec![entries[0].0],
        "只保留 A：它的摘要不在保留前缀里"
    );
}

#[test]
fn rewind_judges_recorded_and_unrecorded_entries_by_their_own_evidence() {
    // (i) 同一保留前缀里既有取代记录落地前的旧行，也有带记录的条目：
    // 旧行按前缀结构判定（摘要 S 存活 ⇒ 保持排除），带记录的条目按自己的摘要判定
    // （它指向的摘要已被 rewind 删掉 ⇒ 清零）。两者互不干扰。
    let entries = vec![
        flag_entry(1, true, false),
        superseded_entry(2, 9),
        flag_entry(3, false, false),
    ];

    assert_eq!(
        rewind_clear_excluded_ids(&entries, 3),
        vec![entries[1].0],
        "旧行走保守回退，带记录的条目走精确判定"
    );
}
