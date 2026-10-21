//! OPT-03：模型视图修订号失效覆盖（`transcript_view` 模块的测试）。
//!
//! 修订号是模型视图复用（`stages::model_view`）的唯一失效依据：任何漏掉的写入口
//! 都会让同一次执行准备内的检查点复用旧视图，把已变更内容当成未变更。本文件逐
//! 入口断言「推进 / 不推进」，与 `transcript_test.rs` 分离以遵守 STD-SIZE-001。

use std::collections::HashMap;

use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};

use crate::agent::compact_v2::projection::MessageProjectionDirective;
use crate::messages::{BaseMessage, MessageContent, MessageId};

use super::super::{MessageTranscript, TranscriptRevision};

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai(text: &str) -> BaseMessage {
    BaseMessage::ai(MessageContent::text(text.to_string()))
}

fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
    BaseMessage::tool_result(
        tool_call_id.to_string(),
        MessageContent::text(text.to_string()),
    )
}

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
            source: ReminderSource("revision_test".into()),
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

/// 断言修订号前进（实例身份保持）。
fn assert_revision_advanced(
    transcript: &MessageTranscript,
    previous: TranscriptRevision,
    case: &str,
) -> TranscriptRevision {
    let current = transcript.view_revision();
    assert_eq!(
        current.instance, previous.instance,
        "{case}: 实例身份不得变化"
    );
    assert!(
        current.revision > previous.revision,
        "{case}: 修订号必须前进（旧模型视图一律失效），previous={previous:?} current={current:?}"
    );
    current
}

/// 失效覆盖表：每个改变模型视图的写入入口都必须推进修订号。
#[tokio::test]
async fn test_view_revision_advances_on_model_view_mutations() {
    use peri_acp_types::system_reminder::{ReminderAudience, ReminderDelivery};

    let mut t = MessageTranscript::new();
    let mut rev = t.view_revision();

    // 追加
    let first_id = t.append(make_human("first"));
    rev = assert_revision_advanced(&t, rev, "append");

    t.append_batch(vec![make_ai("batch ai"), make_tool_result("tc-1", "ok")]);
    rev = assert_revision_advanced(&t, rev, "append_batch");

    // 空批次不改变内容：不得为不存在的变更推进修订号
    let before_empty = t.view_revision();
    t.append_batch(vec![]);
    assert_eq!(t.view_revision(), before_empty, "空批次不改变模型视图");

    // reminder 追加与镜像
    t.append_system_reminder(reminder_with(
        "model_note",
        ReminderDelivery::Required,
        vec![ReminderAudience::Model],
    ));
    rev = assert_revision_advanced(&t, rev, "append_system_reminder");

    assert!(t.mirror_committed_reminder(
        MessageId::new(),
        reminder_with(
            "mirrored_note",
            ReminderDelivery::Required,
            vec![ReminderAudience::Model],
        )
    ));
    rev = assert_revision_advanced(&t, rev, "mirror_committed_reminder");

    assert!(t.mirror_committed_payload(PersistedPayload::Message(make_human("mirrored payload"))));
    rev = assert_revision_advanced(&t, rev, "mirror_committed_payload");

    // 同长度替换：消息数量不变也必须失效
    let same_len = t.len();
    t.replace_by_id(make_human("replaced").with_message_id(first_id));
    assert_eq!(t.len(), same_len, "替换不改变消息数量");
    rev = assert_revision_advanced(&t, rev, "replace_by_id");

    // flags 与 projection
    let target = t.entries()[1].id();
    t.set_truncated(target, true);
    rev = assert_revision_advanced(&t, rev, "set_truncated");
    t.set_excluded(target, true);
    rev = assert_revision_advanced(&t, rev, "set_excluded");
    t.set_flags_projection(
        target,
        MessageProjectionDirective {
            policy_version: 2,
            entries: vec![],
            superseded_by: None,
            ..Default::default()
        },
    );
    rev = assert_revision_advanced(&t, rev, "set_flags_projection");
    t.clear_flags(target);
    rev = assert_revision_advanced(&t, rev, "clear_flags");
    t.set_flags_batch(HashMap::from([(
        target,
        MessageFlags {
            truncated: true,
            ..Default::default()
        },
    )]));
    rev = assert_revision_advanced(&t, rev, "set_flags_batch");

    // staging 不进模型视图：暂存与丢弃不推进，提交才推进
    t.stage_ai_message(make_ai("staged ai"));
    assert_eq!(t.view_revision(), rev, "stage_ai_message 不进入模型视图");
    t.stage_tool_result(make_tool_result("tc-staged", "staged result"));
    assert_eq!(t.view_revision(), rev, "stage_tool_result 不进入模型视图");
    t.discard_staged();
    assert_eq!(t.view_revision(), rev, "discard_staged 不改变模型视图");

    t.stage_ai_message(make_ai("staged ai"));
    t.stage_tool_result(make_tool_result("tc-staged", "staged result"));
    t.commit_staged();
    rev = assert_revision_advanced(&t, rev, "commit_staged");

    // Full Compact 生命周期提交（内存应用路径）
    t.apply_compaction_lifecycle_memory(&CompactionChange {
        flag_updates: vec![],
        appended_messages: vec![make_human("summary")],
    });
    rev = assert_revision_advanced(&t, rev, "apply_compaction_lifecycle_memory");

    // Full Compact 完成标记本身不改变模型视图
    t.mark_full_compaction_committed();
    assert_eq!(
        t.view_revision(),
        rev,
        "mark_full_compaction_committed 不改视图"
    );
    assert!(t.full_compaction_committed());
    t.reset_full_compaction_committed();
    assert!(
        !t.full_compaction_committed(),
        "the Full-commit flag is per turn and must reset without changing the view"
    );
    assert_eq!(
        t.view_revision(),
        rev,
        "reset_full_compaction_committed 不改视图"
    );

    // rebuild：整体替换内容，实例身份保留、修订号前进
    let rebuilt = t.rebuild(vec![(make_human("rebuilt"), MessageFlags::default())]);
    assert_revision_advanced(&rebuilt, rev, "rebuild");

    // rewind：截断到既有消息
    let mut rebuilt = rebuilt;
    let keep = rebuilt.append(make_human("keep"));
    rebuilt.append(make_human("drop"));
    let before_rewind = rebuilt.view_revision();
    rebuilt.rewind_to(keep).await.unwrap();
    assert_revision_advanced(&rebuilt, before_rewind, "rewind_to");
}

/// 两个 transcript 实例的实例身份必须不同：Compact 会 `mem::take` 换出实例，
/// 换入的新实例修订号可能与旧实例相同，视图复用必须靠实例身份区分。
#[test]
fn test_transcript_instances_have_distinct_identity() {
    let left = MessageTranscript::new();
    let right = MessageTranscript::new();
    assert_ne!(
        left.view_revision().instance,
        right.view_revision().instance,
        "实例身份必须唯一"
    );
    assert_eq!(
        left.view_revision().revision,
        right.view_revision().revision,
        "构造前提：两个新实例的修订号相同"
    );
}
