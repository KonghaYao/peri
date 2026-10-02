use super::*;

#[test]
fn test_ancestor_flags_can_restore_but_cannot_mutate() {
    let ancestor = BaseMessage::human("parent snapshot");
    let ancestor_id = ancestor.id();
    let mut transcript = MessageTranscript::new().with_ancestor(vec![ancestor]);
    let restored = MessageFlags {
        excluded: true,
        ..Default::default()
    };
    transcript.set_flags_batch(HashMap::from([(ancestor_id, restored.clone())]));
    transcript.set_excluded(ancestor_id, false);
    transcript.set_truncated(ancestor_id, true);
    transcript.set_flags_projection(
        ancestor_id,
        MessageProjectionDirective {
            policy_version: 2,
            entries: vec![],
        },
    );
    transcript.clear_flags(ancestor_id);
    assert_eq!(
        transcript.flags(ancestor_id),
        restored,
        "所有普通flag写入口都必须保留只读祖先快照"
    );
    let own_id = transcript.append(BaseMessage::human("own work"));
    transcript.set_excluded(own_id, true);
    assert!(transcript.flags(own_id).excluded, "own区域仍允许压缩");
}

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

use std::sync::Arc;

use peri_acp_types::store::CompactionChange;

use crate::messages::MessageContent;
use crate::session::test_resources::mock::MockSessionResources;
use crate::session::test_resources::TestSession;

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

// ── 持久化行为（内存门面替身 + 真实 SQLite 夹具）──────────────────────────
//
// 断言以可观察结果为准：落库内容/顺序、投影与 rewind 的实际效果、失败后的 sticky
// 状态与「未保存」如实上报、预算耗尽时写入被拒。真实后端路径另有
// [`TestSession`](crate::session::test_resources::TestSession) 覆盖（真门面 + 真库
// + 活跃 owner，不靠替身自洽）。

fn mock_store() -> Arc<MockSessionResources> {
    MockSessionResources::new()
}

fn persisted_messages(store: &MockSessionResources) -> Vec<BaseMessage> {
    store
        .payloads()
        .iter()
        .filter_map(|payload| payload.as_message().cloned())
        .collect()
}

/// 追加与 flush 之后，真实 SQLite 后端必须逐条可读且顺序不变。
#[tokio::test]
async fn test_flush_persistence_makes_appends_visible() {
    let session = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(session.resources(), session.thread_id.clone());

    transcript.append(make_human("persisted message"));
    transcript.append(make_ai("assistant reply"));
    transcript.flush_persistence().await.unwrap();

    let snapshot = session
        .resources()
        .load_session_snapshot(&session.thread_id)
        .await
        .unwrap();
    let contents: Vec<String> = snapshot
        .payloads
        .iter()
        .filter_map(|payload| payload.as_message())
        .map(|message| message.content())
        .collect();
    assert_eq!(contents, vec!["persisted message", "assistant reply"]);
}

/// 写入失败后 flush 如实报错，且错误是 sticky 的：后续 flush 仍报同一原因；
/// 已经进入内存但未落库的内容不会被说成已保存。
#[tokio::test]
async fn test_flush_persistence_failure_is_sticky() {
    let session = TestSession::open().await;
    let mut transcript = MessageTranscript::new().with_persistence(
        session.read_only_resources().await,
        session.thread_id.clone(),
    );
    transcript.append(make_human("cannot be persisted"));

    let error = transcript.flush_persistence().await.unwrap_err();
    let repeated = transcript.flush_persistence().await.unwrap_err();
    assert_eq!(
        repeated.to_string(),
        error.to_string(),
        "失败必须是 sticky 的"
    );
    assert!(transcript.has_persistence_failure());

    let snapshot = session
        .resources()
        .load_session_snapshot(&session.thread_id)
        .await
        .unwrap();
    assert!(snapshot.payloads.is_empty(), "写入未成立时不得留下半条");
}

/// 预算耗尽：追加在持锁时同步预留，无法预留即 sticky 失败，且**不把 payload 交给
/// 后端**（不接受「先写再说」）。
#[tokio::test]
async fn test_persistence_budget_exhaustion_is_sticky_and_rejects_writes() {
    let store = mock_store();
    let budget = super::persistence::PersistenceBudget::new(1, 8);
    let mut transcript = MessageTranscript::new().with_persistence_budget(
        store.clone(),
        "budget-exhausted".to_string(),
        budget,
    );

    transcript.append(make_human(
        "this payload is larger than the whole byte budget",
    ));
    assert!(
        transcript.has_persistence_failure(),
        "无法预留时必须留下 sticky 失败"
    );
    let error = transcript.flush_persistence().await.unwrap_err();
    assert!(
        error.to_string().contains("budget"),
        "错误必须指出预算耗尽: {error}"
    );
    assert_eq!(store.append_calls(), 0, "被拒的写入不得触达后端");
    assert!(store.payloads().is_empty());
}

/// 没有绑定持久化后端时 flush 是不报错的 no-op（不制造失败，也不声称保存）。
#[tokio::test]
async fn test_flush_persistence_without_backend_is_ok() {
    MessageTranscript::new().flush_persistence().await.unwrap();
}

/// 投影与 rewind 各走一次完整行为：flags 落库、rewind 截断、缓存视图由资源侧维护
/// （不再由调用方逐条 update + 另行 invalidation）。
#[tokio::test]
async fn test_projection_and_rewind_use_single_behaviors() {
    let store = mock_store();
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "projection".to_string());

    let first = transcript.append(make_human("first"));
    let second = transcript.append(make_ai("second"));
    transcript.flush_persistence().await.unwrap();
    assert_eq!(
        store.append_calls(),
        1,
        "同一窗口内的追加应合并为一次批量写入"
    );
    assert_eq!(store.payloads().len(), 2);

    transcript.set_excluded(first, true);
    transcript.flush_persistence().await.unwrap();
    assert!(store.flags(&first).excluded);

    transcript.rewind_to(first).unwrap();
    transcript.flush_persistence().await.unwrap();
    let messages = persisted_messages(&store);
    assert_eq!(messages.len(), 1, "rewind 保留目标本身（KeepThrough）");
    assert_eq!(messages[0].id(), first);
    assert!(messages.iter().all(|message| message.id() != second));
    assert_eq!(store.rewind_calls(), 1);
}

/// compaction 生命周期：一次完整行为提交（摘要、flags、追加消息），成功才改内存；
/// 失败时磁盘事实保留、内存视图不前进，热态标记为不确定（必须冷重载）。
#[tokio::test]
async fn test_commit_compaction_lifecycle_is_atomic_or_uncertain() {
    let store = mock_store();
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "lifecycle".to_string());

    let first = transcript.append(make_human("原始用户消息"));
    transcript.flush_persistence().await.unwrap();

    let summary = make_human("压缩摘要");
    let summary_id = summary.id();
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: vec![(
                first,
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )],
            appended_messages: vec![summary],
        })
        .await
        .unwrap();
    assert_eq!(store.compaction_calls(), 1, "compaction 必须一次提交");
    assert!(store.flags(&first).excluded);
    assert!(persisted_messages(&store)
        .iter()
        .any(|message| message.id() == summary_id));
    assert!(transcript.compaction_commit_state().has_committed());
    assert!(transcript.get(summary_id).is_some(), "成功后才改内存");

    let store2 = mock_store();
    store2.fail_compaction();
    let mut failing =
        MessageTranscript::new().with_persistence(store2.clone(), "lifecycle-fail".to_string());
    let id = failing.append(make_human("keep me"));
    failing.flush_persistence().await.unwrap();
    let extra = make_human("never applied");
    let extra_id = extra.id();
    let result = failing
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: vec![(
                id,
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )],
            appended_messages: vec![extra],
        })
        .await;
    assert!(result.is_err(), "提交失败必须如实返回错误");
    assert!(!store2.flags(&id).excluded, "失败时磁盘事实不得被改");
    assert!(persisted_messages(&store2)
        .iter()
        .all(|message| message.id() != extra_id));
    assert!(failing.get(extra_id).is_none(), "失败时内存不得前进");
    assert!(
        failing.compaction_commit_state().is_uncertain(),
        "失败后热态必须失效（冷重载才能恢复）"
    );
}

/// rebuild 保留持久化绑定：写入继续到同一个后端，顺序与既有历史都不丢。
///
/// rebuild 只替换内存视图（compact 的 canonical 变更由 `commit_compaction_lifecycle`
/// 承担），**不删除**后端已保存的历史——因此这里断言的是「旧消息仍在、新消息按顺序
/// 追加」，而不是「旧 id 消失」。
#[tokio::test]
async fn test_rebuild_keeps_persistence_writer_alive() {
    let store = mock_store();
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "rebuild-writer".to_string());
    let id = transcript.append(make_human("before rebuild"));
    transcript.flush_persistence().await.unwrap();

    let entries: Vec<(BaseMessage, MessageFlags)> = transcript
        .entries()
        .iter()
        .map(|entry| (entry.message().clone(), transcript.flags(entry.id())))
        .collect();
    let mut rebuilt = transcript.rebuild(entries);

    rebuilt.append(make_human("appended after rebuild"));
    rebuilt.flush_persistence().await.unwrap();

    let messages = persisted_messages(&store);
    assert_eq!(
        messages.len(),
        2,
        "rebuild 不重写后端，追加的消息必须持久化"
    );
    assert_eq!(messages[0].id(), id, "既有历史必须保留原 id 与顺序");
    assert_eq!(messages[1].content(), "appended after rebuild");
}

/// 关闭语义：shutdown 先 flush 积压再退出；退出后 flush 报错（通道关闭）。
#[tokio::test]
async fn test_shutdown_persistence_flushes_pending_and_writer_exits() {
    let store = mock_store();
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "shutdown-flush".to_string());
    transcript.append(make_human("last batch before shutdown"));

    transcript.shutdown_persistence();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while transcript.flush_persistence().await.is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown 后 writer 应在超时前 flush 并退出");

    let messages = persisted_messages(&store);
    assert_eq!(messages.len(), 1, "shutdown 前积压的消息必须落库");
    assert_eq!(messages[0].content(), "last batch before shutdown");
}

#[tokio::test]
async fn test_drop_flushes_pending_appends_to_store() {
    let store = mock_store();
    {
        let mut transcript =
            MessageTranscript::new().with_persistence(store.clone(), "drop-flush".to_string());
        transcript.append(make_human("flush on drop"));
        // writer 持有独立 Arc；Drop 只发 Shutdown，detached 收尾
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while persisted_messages(&store).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Drop 后 writer 必须完成收尾写入");

    let messages = persisted_messages(&store);
    assert_eq!(messages[0].content(), "flush on drop");
}

#[tokio::test]
async fn test_drop_without_persistence_is_noop() {
    drop(MessageTranscript::new());
}

#[test]
fn test_new_transcript_is_empty() {
    let t = MessageTranscript::new();
    assert!(t.is_empty());
    assert_eq!(t.len(), 0);
    assert_eq!(t.ancestor_len(), 0);
}

#[test]
fn test_with_ancestor_sets_boundary() {
    let a1 = make_human("ancestor-1");
    let a2 = make_human("ancestor-2");
    let t = MessageTranscript::new().with_ancestor(vec![a1.clone(), a2.clone()]);

    assert_eq!(t.len(), 2);
    assert_eq!(t.ancestor_len(), 2);
    assert!(t.get(a1.id()).is_some());
    assert!(t.get(a2.id()).is_some());
}

#[test]
fn test_loaded_root_history_remains_in_compactable_own_region() {
    let history = vec![
        PersistedPayload::Message(make_human("previous question")),
        PersistedPayload::Message(make_ai("previous answer")),
    ];

    // Mirrors the main-agent Phase 5 history seeding path.
    let transcript = MessageTranscript::new().with_own_payloads(history);

    assert_eq!(transcript.len(), 2);
    assert_eq!(
        transcript.ancestor_len(),
        0,
        "主 Agent 已加载历史是自身 transcript，不得被标成只读 SubAgent ancestor"
    );
}

// ── ID 寻址 ─────────────────────────────────────────────────────────────────

#[test]
fn test_id_indexing_o1_lookup() {
    let mut t = MessageTranscript::new();
    let m1 = make_human("msg-1");
    let m2 = make_human("msg-2");
    let m3 = make_human("msg-3");

    let id1 = t.append(m1);
    let id2 = t.append(m2);
    let id3 = t.append(m3);

    assert_eq!(t.len(), 3);
    // 所有 id 可找到
    assert!(t.get(id1).is_some());
    assert!(t.get(id2).is_some());
    assert!(t.get(id3).is_some());
    // 不存在的 id 返回 None
    let ghost_id = MessageId::new();
    assert!(t.get(ghost_id).is_none());
}

#[test]
fn test_append_returns_correct_id() {
    let mut t = MessageTranscript::new();
    let msg = make_human("hello");
    let id = t.append(msg);
    // 返回的 id 应与消息内部 id 一致
    assert_eq!(t.get(id).unwrap().message().id(), id);
}

#[test]
fn test_append_batch() {
    let mut t = MessageTranscript::new();
    let msgs = vec![make_human("a"), make_human("b"), make_human("c")];
    let ids = t.append_batch(msgs);

    assert_eq!(ids.len(), 3);
    assert_eq!(t.len(), 3);
    // 按 append 顺序存储
    assert_eq!(t.entries()[0].message().content(), "a");
    assert_eq!(t.entries()[1].message().content(), "b");
    assert_eq!(t.entries()[2].message().content(), "c");
}

// ── Staging 两阶段写入 ────────────────────────────────────────────────────

#[test]
fn test_staging_commit_atomic() {
    let mut t = MessageTranscript::new();
    // 先追加一条用户消息
    t.append(make_human("user question"));

    // Stage AI 消息
    let ai_msg = make_ai("thinking...");
    t.stage_ai_message(ai_msg);
    assert!(t.has_staged());
    // Staging 期间主列表不变
    assert_eq!(t.len(), 1);

    // Stage ToolResult
    t.stage_tool_result(make_tool_result("call_1", "result-1"));
    t.stage_tool_result(make_tool_result("call_2", "result-2"));

    // Commit
    t.commit_staged();
    assert!(!t.has_staged());
    // AI + 2 个 ToolResult = 3 条新消息
    assert_eq!(t.len(), 4);
    // 顺序：user → ai → tool1 → tool2
    assert_eq!(t.entries()[1].message().content(), "thinking...");
    assert_eq!(t.entries()[2].message().content(), "result-1");
    assert_eq!(t.entries()[3].message().content(), "result-2");
}

#[test]
fn test_staging_discard() {
    let mut t = MessageTranscript::new();
    t.append(make_human("user question"));

    let ai_msg = make_ai("will be discarded");
    t.stage_ai_message(ai_msg);
    t.stage_tool_result(make_tool_result("call_1", "also discarded"));
    assert!(t.has_staged());

    t.discard_staged();
    assert!(!t.has_staged());
    // 主列表不变
    assert_eq!(t.len(), 1);
}

#[test]
fn test_stage_tool_result_without_ai_message_is_noop() {
    let mut t = MessageTranscript::new();
    t.stage_tool_result(make_tool_result("call_1", "ignored"));
    assert!(!t.has_staged(), "无 AI 消息时 tool_result 应被忽略");
}

#[test]
fn test_stage_ai_message_overwrites_previous_staging() {
    let mut t = MessageTranscript::new();

    let ai1 = make_ai("first ai");
    t.stage_ai_message(ai1);
    t.stage_tool_result(make_tool_result("call_1", "result for first"));

    // 新的 AI 消息覆盖旧的 staging
    let ai2 = make_ai("second ai");
    t.stage_ai_message(ai2);
    // 旧的 tool_results 被丢弃
    t.stage_tool_result(make_tool_result("call_2", "result for second"));

    t.commit_staged();
    assert_eq!(t.len(), 2, "只有 ai2 + tool2，ai1 和 tool1 被丢弃");
    assert_eq!(t.entries()[0].message().content(), "second ai");
    assert_eq!(t.entries()[1].message().content(), "result for second");
}

#[test]
fn test_commit_without_staging_is_noop() {
    let mut t = MessageTranscript::new();
    t.append(make_human("existing"));
    t.commit_staged(); // 无 staging，不应 panic
    assert_eq!(t.len(), 1);
}

// ── 标记系统 ───────────────────────────────────────────────────────────────

#[test]
fn test_truncated_flag() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("truncatable"));
    assert_eq!(t.flags(id), MessageFlags::default());
    assert!(!t.flags(id).truncated);

    t.set_truncated(id, true);
    assert!(t.flags(id).truncated);
    assert!(!t.flags(id).excluded);

    t.set_truncated(id, false);
    assert!(!t.flags(id).truncated);
}

#[test]
fn test_excluded_flag() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("excludable"));

    t.set_excluded(id, true);
    assert!(t.flags(id).excluded);
    assert!(!t.flags(id).truncated);
}

#[test]
fn test_clear_flags() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("flagged"));
    t.set_truncated(id, true);
    t.set_excluded(id, true);

    t.clear_flags(id);
    let f = t.flags(id);
    assert!(!f.truncated);
    assert!(!f.excluded);
}

#[test]
fn test_visible_messages_skips_excluded() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("visible-1"));
    let id2 = t.append(make_human("will-be-excluded"));
    let id3 = t.append(make_human("visible-2"));

    t.set_excluded(id2, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 2, "excluded 消息应被过滤");
    assert_eq!(visible[0].id(), id1);
    assert_eq!(visible[1].id(), id3);
}

#[test]
fn test_visible_messages_keeps_truncated() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("truncated but visible"));
    t.set_truncated(id, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 1, "truncated 消息仍然可见");
}

// ── 特征化测试：visible_messages 过滤 excluded ──────────────────────────

#[test]
fn test_visible_messages_filters_excluded() {
    // visible_messages() 过滤 excluded=true 的消息，保留 truncated/excluded=false
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("visible human"));
    let id2 = t.append(make_ai("excluded ai"));
    let id3 = t.append(make_tool_result("call_1", "excluded tool result"));
    let id4 = t.append(make_human("visible again"));

    // 标记 id2 和 id3 为 excluded
    t.set_excluded(id2, true);
    t.set_excluded(id3, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 2, "excluded 消息被过滤，只保留 2 条");
    assert_eq!(visible[0].id(), id1, "第 1 条应是 visible human");
    assert_eq!(visible[1].id(), id4, "第 2 条应是 visible again");

    // 取消 excluded 后恢复可见
    t.set_excluded(id2, false);
    t.set_excluded(id3, false);
    let visible2 = t.visible_messages();
    assert_eq!(visible2.len(), 4, "取消 excluded 后所有消息应恢复可见");
}

// ── Ancestor 边界 ──────────────────────────────────────────────────────────

#[test]
fn test_ancestor_boundary_is_readonly_concept() {
    let a1 = make_human("ancestor");
    let own = make_human("own message");
    let mut t = MessageTranscript::new().with_ancestor(vec![a1]);

    t.append(own);
    assert_eq!(t.ancestor_len(), 1);
    assert_eq!(t.len(), 2);
}

// ── Rewind ──────────────────────────────────────────────────────────────────

#[test]
fn test_rewind_to_truncates_correctly() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("keep-1"));
    let id2 = t.append(make_human("keep-2"));
    let _id3 = t.append(make_human("will-remove-1"));
    let _id4 = t.append(make_human("will-remove-2"));

    t.rewind_to(id2).unwrap();
    assert_eq!(t.len(), 2, "rewind 后应只保留 id1 + id2");
    assert!(t.get(id1).is_some());
    assert!(t.get(id2).is_some());
}

#[test]
fn test_rewind_clears_staging() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("target"));
    t.append(make_human("after"));

    t.stage_ai_message(make_ai("staged ai"));
    assert!(t.has_staged());

    t.rewind_to(id).unwrap();
    assert!(!t.has_staged(), "rewind 应清空 staging");
    assert_eq!(t.len(), 1);
}

#[test]
fn test_rewind_nonexistent_id_returns_error() {
    let mut t = MessageTranscript::new();
    t.append(make_human("only msg"));
    let ghost_id = MessageId::new();

    let result = t.rewind_to(ghost_id);
    assert!(result.is_err(), "rewind 不存在的 id 应返回错误");
}

#[test]
fn test_rewind_into_ancestor_returns_error() {
    let a1 = make_human("ancestor");
    let mut t = MessageTranscript::new().with_ancestor(vec![a1.clone()]);
    t.append(make_human("own"));

    let result = t.rewind_to(a1.id());
    assert!(result.is_err(), "rewind 到祖先区域应返回错误");
}

// ── Rebuild ───────────────────────────────────────────────────────────────

#[test]
fn test_rebuild_preserves_flags() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("msg-1"));
    let id2 = t.append(make_human("msg-2"));
    t.set_excluded(id1, true);

    // 重建：保留 id1 的 excluded 标记
    let entries = vec![
        (
            t.entries()[0].message().clone(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        ),
        (t.entries()[1].message().clone(), MessageFlags::default()),
    ];

    let t2 = t.rebuild(entries);
    assert_eq!(t2.len(), 2);
    assert!(t2.flags(id1).excluded, "rebuild 后标记应保留");
    assert!(!t2.flags(id2).excluded);
}

#[test]
fn test_rebuild_preserves_ancestor_and_persistence() {
    let mut t = MessageTranscript::new().with_ancestor(vec![make_human("ancestor")]);
    t.append(make_human("own-1"));
    t.append(make_human("own-2"));

    let entries: Vec<(BaseMessage, MessageFlags)> = t
        .entries()
        .iter()
        .map(|e| (e.message().clone(), MessageFlags::default()))
        .collect();

    let t2 = t.rebuild(entries);
    assert_eq!(t2.ancestor_len(), 1, "rebuild 应保留 ancestor_len");
    assert_eq!(t2.len(), 3);
}

#[test]
fn test_rebuild_clears_staging() {
    let mut t = MessageTranscript::new();
    t.append(make_human("msg"));
    t.stage_ai_message(make_ai("staged"));

    let entries = vec![(t.entries()[0].message().clone(), MessageFlags::default())];
    let t2 = t.rebuild(entries);
    assert!(!t2.has_staged(), "rebuild 应清空 staging");
}

// ── set_flags_batch ───────────────────────────────────────────────────────

#[test]
fn test_set_flags_batch() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("msg-1"));
    let id2 = t.append(make_human("msg-2"));
    let id3 = t.append(make_human("msg-3"));

    let mut batch = std::collections::HashMap::new();
    batch.insert(
        id1,
        MessageFlags {
            truncated: true,
            excluded: false,
            ..Default::default()
        },
    );
    batch.insert(
        id2,
        MessageFlags {
            truncated: false,
            excluded: true,
            ..Default::default()
        },
    );
    batch.insert(
        id3,
        MessageFlags {
            truncated: true,
            excluded: true,
            ..Default::default()
        },
    );

    t.set_flags_batch(batch);

    assert!(t.flags(id1).truncated, "id1 truncated");
    assert!(!t.flags(id1).excluded, "id1 not excluded");
    assert!(!t.flags(id2).truncated, "id2 not truncated");
    assert!(t.flags(id2).excluded, "id2 excluded");
    assert!(t.flags(id3).truncated, "id3 truncated");
    assert!(t.flags(id3).excluded, "id3 excluded");
}

#[test]
fn test_set_flags_batch_ignores_default() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("msg"));

    let mut batch = std::collections::HashMap::new();
    batch.insert(id, MessageFlags::default());

    t.set_flags_batch(batch);

    // Default flags should not be stored; flags() returns default
    assert_eq!(t.flags(id), MessageFlags::default());
}

/// 特征化测试：projection directive 通过 MessageFlags 持久化后恢复一致
#[test]
fn test_projection_directive_persists_roundtrip() {
    use crate::agent::compact_v2::projection::{
        MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    };
    use std::collections::HashMap;

    let mut t = MessageTranscript::new();
    let id = t.append(make_human("message with projection"));

    // 设置 projection directive（通过 set_flags_batch 批量设置）
    let directive = MessageProjectionDirective {
        policy_version: 2,
        entries: vec![ProjectionActionEntry {
            message_id: id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactText { max_chars: 100 },
        }],
    };

    let mut batch = HashMap::new();
    batch.insert(
        id,
        MessageFlags {
            truncated: true,
            excluded: false,
            projection: Some(directive.clone()),
        },
    );
    t.set_flags_batch(batch);

    // 验证内存状态
    let flags = t.flags(id);
    assert!(flags.truncated);
    assert!(!flags.excluded);
    assert!(flags.projection.is_some(), "projection 应被设置");
    let proj = flags.projection.as_ref().unwrap();
    assert_eq!(proj.policy_version, 2);
    assert_eq!(proj.entries.len(), 1);
    assert_eq!(
        proj.entries[0].action,
        ProjectionAction::CompactText { max_chars: 100 }
    );

    // 验证 rebuild 保留 projection
    let entries: Vec<(BaseMessage, MessageFlags)> = t
        .entries()
        .iter()
        .map(|e| {
            let fid = e.id();
            let mut flags = t.flags(fid);
            if fid == id {
                flags.projection = Some(directive.clone());
            }
            (e.message().clone(), flags)
        })
        .collect();

    let t2 = t.rebuild(entries);
    let flags2 = t2.flags(id);
    assert!(flags2.truncated);
    assert!(flags2.projection.is_some(), "rebuild 后 projection 应保留");
    assert_eq!(flags2.projection.as_ref().unwrap().policy_version, 2);
}

#[test]
fn test_projection_directive_none_when_not_set() {
    // 旧行为：不设置 projection 应为 None
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("plain message"));

    t.set_truncated(id, true);

    let flags = t.flags(id);
    assert!(flags.truncated);
    assert!(!flags.excluded);
    assert!(flags.projection.is_none(), "未设置 projection 应为 None");

    // 序列化为 JSON 验证向后兼容
    let json = serde_json::to_string(&flags).unwrap();
    assert!(
        !json.contains("projection"),
        "JSON 应不含 projection 字段（skip_serializing_if）"
    );
}
