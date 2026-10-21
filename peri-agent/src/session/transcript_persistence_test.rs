//! 异步持久化行为：落库内容与顺序、sticky 失败、预算有界、压缩生命周期落盘、
//! shutdown / Drop 排空。

use super::*;

use std::sync::Arc;

use peri_acp_types::store::CompactionChange;

use crate::session::test_resources::mock::MockSessionResources;
use crate::session::test_resources::TestSession;

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

    transcript.rewind_to(first).await.unwrap();
    transcript.flush_persistence().await.unwrap();
    let messages = persisted_messages(&store);
    assert_eq!(messages.len(), 1, "rewind 保留目标本身（KeepThrough）");
    assert_eq!(messages[0].id(), first);
    assert!(messages.iter().all(|message| message.id() != second));
    assert_eq!(store.rewind_calls(), 1);
}

#[tokio::test]
async fn test_rewind_known_failure_preserves_prefix_suffix_and_revision() {
    let bound = TestSession::open().await;
    let messages = vec![make_human("keep"), make_ai("suffix")];
    let keep = messages[0].id();
    bound
        .resources()
        .append_history(
            &bound.thread_id,
            &messages
                .iter()
                .cloned()
                .map(PersistedPayload::Message)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new()
        .with_own_payloads(
            messages
                .into_iter()
                .map(PersistedPayload::Message)
                .collect(),
        )
        .with_persistence(bound.read_only_resources().await, bound.thread_id.clone());
    let revision = transcript.view_revision();
    let error = transcript.rewind_to(keep).await.unwrap_err();
    assert!(!error.to_string().is_empty());
    assert_eq!(transcript.len(), 2);
    assert_eq!(transcript.view_revision(), revision);
    assert!(!transcript.compaction_commit_state().is_uncertain());
    assert_eq!(
        bound
            .resources()
            .load_session_snapshot(&bound.thread_id)
            .await
            .unwrap()
            .payloads
            .len(),
        2
    );
}

/// compaction 生命周期：一次完整行为提交（摘要、flags、追加消息），成功才改内存；
/// 明确失败时磁盘事实保留、内存视图不前进；未知终态由独立用例要求冷重载。
#[tokio::test]
async fn test_commit_compaction_lifecycle_known_failure_retains_old_view() {
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
        !failing.compaction_commit_state().is_uncertain(),
        "明确未提交必须保留上一份完整视图"
    );
}

/// Full Compact 提交成功后收缩：被排除的 own 条目连同 id 索引与 flags 一起释放，
/// 祖先区、System 与未排除 own 条目按原顺序保留；三个容器按保留数重建（旧分配释放）。
#[tokio::test]
async fn test_full_commit_releases_excluded_own_entries() {
    let store = mock_store();
    let ancestor = make_human("ancestor snapshot");
    let ancestor_id = ancestor.id();
    let mut transcript = MessageTranscript::new()
        .with_ancestor(vec![ancestor])
        .with_persistence(store.clone(), "shrink".to_string());

    // 条目数要明显多于保留数，容量差异才能证明旧分配确实被释放。
    let system_id = transcript.append(BaseMessage::system("session identity"));
    let kept_id = transcript.append(make_ai("kept history"));
    transcript.set_truncated(kept_id, true);
    let released_ids: Vec<MessageId> = (0..24)
        .map(|idx| transcript.append(make_human(&format!("excluded {idx}"))))
        .collect();
    transcript.flush_persistence().await.unwrap();

    let entries_capacity_before = transcript.entries.capacity();
    let id_index_capacity_before = transcript.id_index.capacity();
    let flags_capacity_before = transcript.flags.capacity();

    let summary = make_human("compaction summary");
    let summary_id = summary.id();
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: released_ids
                .iter()
                .map(|id| {
                    (
                        *id,
                        MessageFlags {
                            excluded: true,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            appended_messages: vec![summary],
        })
        .await
        .unwrap();

    // 保留：祖先、System、未排除条目与本次追加的摘要，顺序不变。
    let kept_ids: Vec<MessageId> = transcript
        .entries()
        .iter()
        .map(TranscriptEntry::id)
        .collect();
    assert_eq!(
        kept_ids,
        vec![ancestor_id, system_id, kept_id, summary_id],
        "收缩只释放被排除的 own 条目，并保持剩余顺序"
    );

    // 释放：条目、id 索引与 flags 都不留痕。
    for id in &released_ids {
        assert!(transcript.get(*id).is_none(), "被排除条目必须被释放");
        assert!(
            !transcript.id_index.contains_key(id),
            "被释放条目的下标必须一并移除"
        );
        assert!(
            !transcript.flags.contains_key(id),
            "被释放条目的标记必须一并移除"
        );
        assert!(transcript.get_flags(*id).is_none());
    }
    assert!(
        transcript.flags(kept_id).truncated,
        "未排除条目的标记不得被收缩改写"
    );

    // 有效集合等于剩余条目，不含任何被释放 id。
    let active_ids: Vec<MessageId> = transcript
        .active_payloads()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(active_ids, kept_ids);

    // 容量按保留数重建：不再为被释放条目留位。
    assert!(entries_capacity_before > kept_ids.len());
    assert_eq!(transcript.entries.capacity(), kept_ids.len());
    assert!(
        transcript.id_index.capacity() < id_index_capacity_before,
        "id 索引容量必须随释放下降（before={id_index_capacity_before}, after={}）",
        transcript.id_index.capacity()
    );
    assert!(
        transcript.flags.capacity() < released_ids.len(),
        "flags 表重建后不得再为被释放条目留容量（before={flags_capacity_before}, after={}）",
        transcript.flags.capacity()
    );
}

/// 防御性边界：System 条目即使带 `excluded` 也不因收缩消失（当前 Full 不排除它们，
/// 这条锁住的是「收缩只释放被明确排除的非 System own 条目」）。
#[tokio::test]
async fn test_full_commit_keeps_excluded_system_entry() {
    let store = mock_store();
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "shrink-system".to_string());
    let system_id = transcript.append(BaseMessage::system("session identity"));
    let dropped = transcript.append(make_human("excluded"));
    transcript.flush_persistence().await.unwrap();

    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: vec![
                (
                    system_id,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
                (
                    dropped,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
            ],
            appended_messages: Vec::new(),
        })
        .await
        .unwrap();

    assert!(transcript.get(system_id).is_some(), "System 条目必须保留");
    assert!(transcript.get(dropped).is_none(), "非 System 条目必须释放");
    assert!(
        transcript.flags(system_id).excluded,
        "保留条目的标记原样保留"
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

/// 提交终态未知时不发布任何候选修订，也不允许下一次 compact 假装回滚成功。
#[tokio::test]
async fn test_compaction_unknown_outcome_requires_reload() {
    let store = mock_store();
    let mut transcript = MessageTranscript::new().with_persistence(store.clone(), "unknown".into());
    let id = transcript.append(make_human("old committed view"));
    transcript.flush_persistence().await.unwrap();
    let revision = transcript.view_revision();
    store.uncertain_compaction();
    let error = transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: vec![(
                id,
                MessageFlags {
                    truncated: true,
                    ..Default::default()
                },
            )],
            appended_messages: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unknown"));
    assert_eq!(transcript.view_revision(), revision);
    assert!(!transcript.flags(id).truncated);
    assert!(transcript.compaction_commit_state().is_uncertain());
    let error = transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: Vec::new(),
            appended_messages: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reload"));
    assert_eq!(store.compaction_calls(), 1);
}
