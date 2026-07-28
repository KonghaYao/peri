//! Tests for sqlite_store

use std::sync::Arc;

use crate::session::MessageTranscript;
use crate::thread::CompactionLifecycle;
use tempfile::tempdir;

use super::*;

async fn make_store() -> (SqliteThreadStore, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let store = SqliteThreadStore::new(dir.path().join("test.db"))
        .await
        .unwrap();
    (store, dir)
}

fn make_persistent_transcript(
    store: Arc<dyn ThreadStore>,
    thread_id: ThreadId,
) -> MessageTranscript {
    MessageTranscript::new().with_persistence(store, thread_id)
}

#[tokio::test]
async fn test_sqlite_store_flush_persistence_makes_messages_and_flags_readable() {
    let (store, _dir) = make_store().await;
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(store);
    let mut transcript = make_persistent_transcript(store.clone(), thread_id.clone());

    let message_id = transcript.append(BaseMessage::human("durable message"));
    transcript.set_truncated(message_id, true);
    transcript.set_excluded(message_id, true);
    transcript.flush_persistence().await.unwrap();

    let messages = store.load_messages(&thread_id).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id(), message_id);
    assert_eq!(messages[0].content(), "durable message");

    let flags = store.load_message_flags(&thread_id).await.unwrap();
    assert!(flags[&message_id].truncated);
    assert!(flags[&message_id].excluded);
}

#[tokio::test]
async fn test_create_append_load() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();

    let msgs = vec![BaseMessage::human("Hello"), BaseMessage::ai("Hi there")];
    store.append_messages(&id, &msgs).await.unwrap();

    let loaded = store.load_messages(&id).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].content(), "Hello");
    assert_eq!(loaded[1].content(), "Hi there");
}

#[tokio::test]
async fn test_list_threads_order() {
    let (store, _dir) = make_store().await;

    let m1 = ThreadMeta::new("/a");
    let id1 = store.create_thread(m1).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let m2 = ThreadMeta::new("/b");
    let id2 = store.create_thread(m2).await.unwrap();

    // 给 id2 追加消息，更新 updated_at
    store
        .append_messages(&id2, &[BaseMessage::human("msg")])
        .await
        .unwrap();

    let list = store.list_threads().await.unwrap();
    assert_eq!(list.len(), 2);
    // id2 updated_at 更新，应排在第一位
    assert_eq!(list[0].id, id2);
    assert_eq!(list[1].id, id1);
}

#[tokio::test]
async fn test_delete_thread_cascade() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("msg")])
        .await
        .unwrap();

    store.delete_thread(&id).await.unwrap();

    // 消息应该被级联删除
    let msgs = store.load_messages(&id).await;
    // 线程不存在时 load_messages 应返回空（因为 SELECT 无结果）
    assert!(msgs.unwrap().is_empty());

    // 元数据应不存在
    let meta_result = store.load_meta(&id).await;
    assert!(meta_result.is_err());
}

#[tokio::test]
async fn test_message_order_after_multiple_appends() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();

    store
        .append_messages(&id, &[BaseMessage::human("msg1")])
        .await
        .unwrap();
    store
        .append_messages(&id, &[BaseMessage::ai("reply1")])
        .await
        .unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("msg2")])
        .await
        .unwrap();

    let loaded = store.load_messages(&id).await.unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].content(), "msg1");
    assert_eq!(loaded[1].content(), "reply1");
    assert_eq!(loaded[2].content(), "msg2");
}

#[tokio::test]
async fn test_title_auto_set() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();

    store
        .append_messages(&id, &[BaseMessage::human("这是一条测试消息")])
        .await
        .unwrap();

    let loaded_meta = store.load_meta(&id).await.unwrap();
    assert!(loaded_meta.title.is_some());
    assert!(loaded_meta.title.unwrap().contains("这是一条测试消息"));
}

#[tokio::test]
async fn test_update_title() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();

    store.update_title(&id, "new title").await.unwrap();
    let loaded = store.load_meta(&id).await.unwrap();
    assert_eq!(loaded.title.as_deref(), Some("new title"));
}

#[tokio::test]
async fn test_update_title_updates_timestamp() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();

    let before = store.load_meta(&id).await.unwrap().updated_at;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    store.update_title(&id, "updated").await.unwrap();
    let after = store.load_meta(&id).await.unwrap().updated_at;
    assert!(
        after > before,
        "updated_at should be newer after update_title"
    );
}

// ── 新增：子线程创建和列表 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_child_thread_create_and_list() {
    let (store, _dir) = make_store().await;
    // 创建父线程
    let parent_meta = ThreadMeta::new("/project");
    let parent_id = store.create_thread(parent_meta).await.unwrap();

    // 创建子线程
    let mut child_meta = ThreadMeta::new("/project");
    child_meta.parent_thread_id = Some(parent_id.clone());
    child_meta.hidden = true;
    let child_id = store.create_thread(child_meta).await.unwrap();

    // list_child_threads 应返回子线程
    let children = store.list_child_threads(&parent_id).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child_id);
    assert_eq!(
        children[0].parent_thread_id.as_deref(),
        Some(parent_id.as_str())
    );

    // 子线程的 meta 应正确读取 parent_thread_id 和 hidden
    let child_meta_loaded = store.load_meta(&child_id).await.unwrap();
    assert_eq!(
        child_meta_loaded.parent_thread_id.as_deref(),
        Some(parent_id.as_str())
    );
    assert!(child_meta_loaded.hidden);
}

#[tokio::test]
async fn test_session_threads_recursive() {
    let (store, _dir) = make_store().await;
    // L1 根线程
    let l1_id = store.create_thread(ThreadMeta::new("/root")).await.unwrap();
    // L2 子线程
    let mut l2_meta = ThreadMeta::new("/root");
    l2_meta.parent_thread_id = Some(l1_id.clone());
    l2_meta.hidden = true;
    let l2_id = store.create_thread(l2_meta).await.unwrap();
    // L3 孙线程
    let mut l3_meta = ThreadMeta::new("/root");
    l3_meta.parent_thread_id = Some(l2_id.clone());
    l3_meta.hidden = true;
    let l3_id = store.create_thread(l3_meta).await.unwrap();

    // 从 L1 根出发应递归获取全部 3 级
    let session = store.list_session_threads(&l1_id).await.unwrap();
    assert_eq!(session.len(), 3);
    let ids: Vec<&str> = session.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&l1_id.as_str()));
    assert!(ids.contains(&l2_id.as_str()));
    assert!(ids.contains(&l3_id.as_str()));
}

#[tokio::test]
async fn test_update_thread_status() {
    use crate::thread::AgentStatus;
    let (store, _dir) = make_store().await;
    let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();

    // 默认 active
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Active);

    // 更新为 done
    store.update_thread_status(&id, "done").await.unwrap();
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Done);

    // 更新为 error
    store.update_thread_status(&id, "error").await.unwrap();
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Error);
}

#[tokio::test]
async fn test_update_thread_status_rejects_illegal_string() {
    // 关键约束：非法状态字符串不应静默 fallback，必须返回错误
    let (store, _dir) = make_store().await;
    let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let result = store.update_thread_status(&id, "running").await;
    assert!(result.is_err(), "非法 agent_status 字符串应被拒绝");
    // 状态保持不变（active）
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.agent_status, crate::thread::AgentStatus::Active);
}

#[tokio::test]
async fn test_load_context_without_parent() {
    let (store, _dir) = make_store().await;
    let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();

    let msgs = vec![
        BaseMessage::human("hello"),
        BaseMessage::ai("world"),
        BaseMessage::human("how are you"),
    ];
    store.append_messages(&id, &msgs).await.unwrap();

    // 无父线程，load_context 应返回自身全部消息
    let ctx = store.load_context(&id).await.unwrap();
    assert_eq!(ctx.len(), 3);
    assert_eq!(ctx[0].content(), "hello");
    assert_eq!(ctx[1].content(), "world");
    assert_eq!(ctx[2].content(), "how are you");

    // 第二次调用应命中缓存（cached_context 已写入）
    let ctx2 = store.load_context(&id).await.unwrap();
    assert_eq!(ctx2.len(), 3);
}

#[tokio::test]
async fn test_load_context_with_snapshot() {
    let (store, _dir) = make_store().await;
    // 父线程 + 3 条消息
    let parent_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let parent_msgs = vec![
        BaseMessage::human("p1"),
        BaseMessage::ai("p2"),
        BaseMessage::human("p3"),
    ];
    store
        .append_messages(&parent_id, &parent_msgs)
        .await
        .unwrap();

    // 快照截止到第 2 条消息（p2）的 message_id
    let parent_loaded = store.load_messages(&parent_id).await.unwrap();
    let snapshot_msg_id = parent_loaded[1].id().as_uuid().to_string();

    // 更新父线程的 snapshot_at_message_id
    let mut parent_meta = store.load_meta(&parent_id).await.unwrap();
    parent_meta.snapshot_at_message_id = Some(snapshot_msg_id.clone());
    store.update_meta(&parent_id, parent_meta).await.unwrap();

    // 创建子线程
    let mut child_meta = ThreadMeta::new("/tmp");
    child_meta.parent_thread_id = Some(parent_id.clone());
    child_meta.hidden = true;
    let child_id = store.create_thread(child_meta).await.unwrap();

    let child_msgs = vec![BaseMessage::human("c1"), BaseMessage::ai("c2")];
    store.append_messages(&child_id, &child_msgs).await.unwrap();

    // load_context 应返回：父线程前 2 条 + 子线程全部 2 条 = 4 条
    let ctx = store.load_context(&child_id).await.unwrap();
    assert_eq!(ctx.len(), 4, "应包含父线程快照 2 条 + 子线程 2 条");
    assert_eq!(ctx[0].content(), "p1");
    assert_eq!(ctx[1].content(), "p2");
    assert_eq!(ctx[2].content(), "c1");
    assert_eq!(ctx[3].content(), "c2");
}

#[tokio::test]
async fn test_cached_context_invalidation() {
    let (store, _dir) = make_store().await;
    let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("hello")])
        .await
        .unwrap();

    // 首次加载产生缓存
    let ctx = store.load_context(&id).await.unwrap();
    assert_eq!(ctx.len(), 1);

    // 验证缓存已写入
    let meta = store.load_meta(&id).await.unwrap();
    assert!(meta.cached_context.is_some());

    // 清除缓存
    store.invalidate_context_cache(&id).await.unwrap();
    let meta = store.load_meta(&id).await.unwrap();
    assert!(
        meta.cached_context.is_none(),
        "清除缓存后 cached_context 应为 None"
    );

    // 再次加载仍然正常工作（从零重建）
    let ctx2 = store.load_context(&id).await.unwrap();
    assert_eq!(ctx2.len(), 1);
    assert_eq!(ctx2[0].content(), "hello");
}

#[tokio::test]
async fn test_list_threads_excludes_hidden() {
    let (store, _dir) = make_store().await;

    // 创建普通线程
    let visible_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();

    // 创建 hidden 的子 agent 线程
    let mut hidden_meta = ThreadMeta::new("/tmp");
    hidden_meta.parent_thread_id = Some(visible_id.clone());
    hidden_meta.hidden = true;
    let _hidden_id = store.create_thread(hidden_meta).await.unwrap();

    // list_threads 只返回非 hidden 的线程
    let list = store.list_threads().await.unwrap();
    assert_eq!(list.len(), 1, "hidden 线程不应出现在列表中");
    assert_eq!(list[0].id, visible_id);
}

#[tokio::test]
async fn test_load_context_three_level_nesting() {
    let (store, _dir) = make_store().await;

    // L1 根线程：3 条消息，快照到第 2 条
    let l1_id = store
        .create_thread(ThreadMeta::new("/project"))
        .await
        .unwrap();
    let l1_msgs = vec![
        BaseMessage::human("L1-a"),
        BaseMessage::ai("L1-b"),
        BaseMessage::human("L1-c"),
    ];
    store.append_messages(&l1_id, &l1_msgs).await.unwrap();
    let l1_loaded = store.load_messages(&l1_id).await.unwrap();
    let l1_snap = l1_loaded[1].id().as_uuid().to_string();
    let mut l1_meta = store.load_meta(&l1_id).await.unwrap();
    l1_meta.snapshot_at_message_id = Some(l1_snap);
    store.update_meta(&l1_id, l1_meta).await.unwrap();

    // L2 子线程：2 条消息，快照到第 1 条
    let mut l2_meta = ThreadMeta::new("/project");
    l2_meta.parent_thread_id = Some(l1_id.clone());
    l2_meta.hidden = true;
    let l2_id = store.create_thread(l2_meta).await.unwrap();
    let l2_msgs = vec![BaseMessage::human("L2-a"), BaseMessage::ai("L2-b")];
    store.append_messages(&l2_id, &l2_msgs).await.unwrap();
    let l2_loaded = store.load_messages(&l2_id).await.unwrap();
    let l2_snap = l2_loaded[0].id().as_uuid().to_string();
    let mut l2_meta_loaded = store.load_meta(&l2_id).await.unwrap();
    l2_meta_loaded.snapshot_at_message_id = Some(l2_snap);
    store.update_meta(&l2_id, l2_meta_loaded).await.unwrap();

    // L3 孙线程：1 条消息，无快照
    let mut l3_meta = ThreadMeta::new("/project");
    l3_meta.parent_thread_id = Some(l2_id.clone());
    l3_meta.hidden = true;
    let l3_id = store.create_thread(l3_meta).await.unwrap();
    let l3_msgs = vec![BaseMessage::human("L3-a")];
    store.append_messages(&l3_id, &l3_msgs).await.unwrap();

    // load_context(L3) 应返回：L1 快照 2 条 + L2 快照 1 条 + L3 全部 1 条 = 4 条
    let ctx = store.load_context(&l3_id).await.unwrap();
    assert_eq!(
        ctx.len(),
        4,
        "三层嵌套应返回 L1(2) + L2(1) + L3(1) = 4 条消息"
    );
    assert_eq!(ctx[0].content(), "L1-a");
    assert_eq!(ctx[1].content(), "L1-b");
    assert_eq!(ctx[2].content(), "L2-a");
    assert_eq!(ctx[3].content(), "L3-a");
}

#[tokio::test]
async fn test_update_and_load_message_flags() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();

    let msgs = vec![
        BaseMessage::human("msg1"),
        BaseMessage::ai("msg2"),
        BaseMessage::human("msg3"),
    ];
    store.append_messages(&id, &msgs).await.unwrap();

    // Set flags: msg1 truncated, msg2 excluded, msg3 no flags
    store
        .update_message_flags(
            &msgs[0].id(),
            &MessageFlags {
                truncated: true,
                excluded: false,
                projection: None,
            },
        )
        .await
        .unwrap();
    store
        .update_message_flags(
            &msgs[1].id(),
            &MessageFlags {
                truncated: false,
                excluded: true,
                projection: None,
            },
        )
        .await
        .unwrap();

    let flags = store.load_message_flags(&id).await.unwrap();
    assert_eq!(flags.len(), 2, "only 2 messages have non-default flags");
    assert!(flags[&msgs[0].id()].truncated, "msg1 should be truncated");
    assert!(
        !flags[&msgs[0].id()].excluded,
        "msg1 should not be excluded"
    );
    assert!(
        !flags[&msgs[1].id()].truncated,
        "msg2 should not be truncated"
    );
    assert!(flags[&msgs[1].id()].excluded, "msg2 should be excluded");
}

#[tokio::test]
async fn test_load_message_flags_empty_when_no_flags() {
    let (store, _dir) = make_store().await;
    let meta = ThreadMeta::new("/tmp");
    let id = store.create_thread(meta).await.unwrap();

    let msgs = vec![BaseMessage::human("hello"), BaseMessage::ai("world")];
    store.append_messages(&id, &msgs).await.unwrap();

    let flags = store.load_message_flags(&id).await.unwrap();
    assert!(flags.is_empty(), "no flags set, should return empty map");
}

// ── 特征化测试：UpdateFlags 持久化后可恢复 ───────────────────────────────

#[tokio::test]
async fn test_update_message_flags_persists() {
    // UpdateFlags 写入 DB 后，通过新 store 实例可恢复
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("persist_test.db");

    // 第一步：创建 store，写消息，设 flags
    let msg_id = {
        let store = SqliteThreadStore::new(db_path.clone()).await.unwrap();
        let meta = ThreadMeta::new("/tmp");
        let tid = store.create_thread(meta).await.unwrap();

        let msgs = vec![
            BaseMessage::human("hello"),
            BaseMessage::ai("world"),
            BaseMessage::human("howdy"),
        ];
        store.append_messages(&tid, &msgs).await.unwrap();

        // 标记 msg0 truncated, msg1 excluded
        store
            .update_message_flags(
                &msgs[0].id(),
                &MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: None,
                },
            )
            .await
            .unwrap();
        store
            .update_message_flags(
                &msgs[1].id(),
                &MessageFlags {
                    truncated: false,
                    excluded: true,
                    projection: None,
                },
            )
            .await
            .unwrap();

        // 记录 msg0 id 和 thread id 用于后续验证
        let id = msgs[0].id();
        (id, msgs[1].id(), tid)
    }; // store dropped here, DB connection closed

    // 第二步：用新 store 重新打开同一 DB，验证 flags 持久化
    {
        let store = SqliteThreadStore::new(db_path).await.unwrap();
        let tid = &msg_id.2;

        let flags = store.load_message_flags(tid).await.unwrap();
        assert_eq!(flags.len(), 2, "持久化后应有 2 条非默认 flag");

        // msg0: truncated=true, excluded=false
        assert!(flags[&msg_id.0].truncated, "msg0 应是 truncated");
        assert!(!flags[&msg_id.0].excluded, "msg0 不应是 excluded");

        // msg1: truncated=false, excluded=true
        assert!(!flags[&msg_id.1].truncated, "msg1 不应是 truncated");
        assert!(flags[&msg_id.1].excluded, "msg1 应是 excluded");
    }
}

/// 特征化测试：projection JSON 跨 store 实例恢复
#[tokio::test]
async fn test_update_message_flags_persists_projection() {
    use crate::agent::compact_v2::projection::{
        MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    };

    // 构造一个含有 projection directive 的 MessageFlags
    let directive = MessageProjectionDirective {
        policy_version: 1,
        entries: vec![ProjectionActionEntry {
            message_id: crate::messages::MessageId::new(),
            target: ProjectionTarget::Message,
            action: ProjectionAction::Exclude,
        }],
    };

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("proj_test.db");

    // 第一步：写入 projection flag
    let (msg_id, tid) = {
        let store = SqliteThreadStore::new(db_path.clone()).await.unwrap();
        let meta = ThreadMeta::new("/tmp");
        let tid = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("projected message")];
        store.append_messages(&tid, &msgs).await.unwrap();

        let mid = msgs[0].id();
        store
            .update_message_flags(
                &mid,
                &MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: Some(directive.clone()),
                },
            )
            .await
            .unwrap();

        (mid, tid)
    }; // store dropped

    // 第二步：新 store 恢复
    {
        let store = SqliteThreadStore::new(db_path).await.unwrap();
        let flags = store.load_message_flags(&tid).await.unwrap();
        assert_eq!(flags.len(), 1, "应有 1 条非默认 flag");
        let flag = &flags[&msg_id];
        assert!(flag.truncated, "truncated 应为 true");
        assert!(!flag.excluded, "excluded 应为 false");
        assert!(flag.projection.is_some(), "projection 应不为 None");
        let restored = flag.projection.as_ref().unwrap();
        assert_eq!(restored.policy_version, 1);
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].action, ProjectionAction::Exclude);
    }
}

#[tokio::test]
async fn test_commit_compaction_lifecycle_persists_flags_and_appended_messages_in_order() {
    let (store, _dir) = make_store().await;
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let original_messages = vec![
        BaseMessage::human("原始用户消息"),
        BaseMessage::ai("原始助手回复"),
    ];
    store
        .append_messages(&thread_id, &original_messages)
        .await
        .unwrap();

    let summary = BaseMessage::human("压缩摘要");
    let reinject = BaseMessage::human("重新注入的用户上下文");
    let lifecycle = CompactionLifecycle {
        flag_updates: vec![
            (
                original_messages[0].id(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            ),
            (
                original_messages[1].id(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            ),
        ],
        appended_messages: vec![summary.clone(), reinject.clone()],
    };

    store
        .commit_compaction_lifecycle(&thread_id, &lifecycle)
        .await
        .unwrap();

    let messages = store.load_messages(&thread_id).await.unwrap();
    assert_eq!(
        messages.len(),
        4,
        "生命周期提交应持久化原始与追加的全部消息"
    );
    assert_eq!(
        messages[0].id(),
        original_messages[0].id(),
        "原始第一条消息顺序不变"
    );
    assert_eq!(
        messages[1].id(),
        original_messages[1].id(),
        "原始第二条消息顺序不变"
    );
    assert_eq!(messages[2].id(), summary.id(), "摘要应在原始消息之后追加");
    assert_eq!(
        messages[3].id(),
        reinject.id(),
        "重新注入消息应紧随摘要追加"
    );

    let flags = store.load_message_flags(&thread_id).await.unwrap();
    assert_eq!(flags.len(), 2, "两条 excluded 标记都应落库");
    assert!(
        flags[&original_messages[0].id()].excluded,
        "第一条原始消息应被 excluded"
    );
    assert!(
        flags[&original_messages[1].id()].excluded,
        "第二条原始消息应被 excluded"
    );
}

#[tokio::test]
async fn test_commit_compaction_lifecycle_rolls_back_flags_and_appends_when_message_is_missing() {
    let (store, _dir) = make_store().await;
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let original_messages = vec![
        BaseMessage::human("回滚前的用户消息"),
        BaseMessage::ai("回滚前的助手回复"),
    ];
    store
        .append_messages(&thread_id, &original_messages)
        .await
        .unwrap();

    let summary = BaseMessage::human("不应落库的压缩摘要");
    let lifecycle = CompactionLifecycle {
        flag_updates: vec![
            (
                original_messages[0].id(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            ),
            (
                crate::messages::MessageId::new(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            ),
        ],
        appended_messages: vec![summary.clone()],
    };

    let error = store
        .commit_compaction_lifecycle(&thread_id, &lifecycle)
        .await
        .unwrap_err();
    assert!(
        !error.to_string().is_empty(),
        "不存在的 MessageId 必须使整个生命周期提交失败"
    );

    let flags = store.load_message_flags(&thread_id).await.unwrap();
    assert!(
        !flags.contains_key(&original_messages[0].id()),
        "事务回滚后有效消息必须保持 default flags"
    );
    let messages = store.load_messages(&thread_id).await.unwrap();
    assert_eq!(messages.len(), 2, "事务回滚后不应追加摘要");
    assert!(
        messages.iter().all(|message| message.id() != summary.id()),
        "事务回滚后摘要不得出现"
    );
}
