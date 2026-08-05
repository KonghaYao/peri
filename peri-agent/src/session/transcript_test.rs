use super::*;
use std::sync::{Arc, Mutex};

use crate::messages::MessageContent;
use crate::thread::{
    CompactionLifecycle, FilesystemThreadStore, SqliteThreadStore, ThreadId, ThreadMeta,
    ThreadStore,
};
use anyhow::Result;
use async_trait::async_trait;
use tempfile::tempdir;

struct FaultInjectingStore {
    fail_on: Vec<usize>,
    fail_flag_on: Vec<usize>,
    fail_invalidation: bool,
    append_count: Mutex<usize>,
    flag_count: Mutex<usize>,
    invalidation_count: Mutex<usize>,
    messages: Mutex<Vec<BaseMessage>>,
}

impl FaultInjectingStore {
    fn new(fail_on: impl IntoIterator<Item = usize>) -> Self {
        Self::with_failures(fail_on, [], false)
    }

    fn with_failures(
        fail_on: impl IntoIterator<Item = usize>,
        fail_flag_on: impl IntoIterator<Item = usize>,
        fail_invalidation: bool,
    ) -> Self {
        Self {
            fail_on: fail_on.into_iter().collect(),
            fail_flag_on: fail_flag_on.into_iter().collect(),
            fail_invalidation,
            append_count: Mutex::new(0),
            flag_count: Mutex::new(0),
            invalidation_count: Mutex::new(0),
            messages: Mutex::new(Vec::new()),
        }
    }

    fn messages(&self) -> Vec<BaseMessage> {
        self.messages.lock().unwrap().clone()
    }
}

#[async_trait]
impl ThreadStore for FaultInjectingStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        Ok(meta.id)
    }

    async fn append_messages(&self, _id: &ThreadId, msgs: &[BaseMessage]) -> Result<()> {
        for message in msgs {
            let mut append_count = self.append_count.lock().unwrap();
            *append_count += 1;
            if self.fail_on.contains(&*append_count) {
                anyhow::bail!("deterministic injected error on append {}", *append_count);
            }
            self.messages.lock().unwrap().push(message.clone());
        }
        Ok(())
    }

    async fn load_messages(&self, _id: &ThreadId) -> Result<Vec<BaseMessage>> {
        Ok(self.messages())
    }

    async fn load_meta(&self, _id: &ThreadId) -> Result<ThreadMeta> {
        Ok(ThreadMeta::new("/test"))
    }

    async fn update_meta(&self, _id: &ThreadId, _meta: ThreadMeta) -> Result<()> {
        Ok(())
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        Ok(Vec::new())
    }

    async fn delete_thread(&self, _id: &ThreadId) -> Result<()> {
        Ok(())
    }

    async fn load_context(&self, _thread_id: &ThreadId) -> Result<Vec<BaseMessage>> {
        Ok(Vec::new())
    }

    async fn list_child_threads(&self, _parent_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        Ok(Vec::new())
    }

    async fn list_session_threads(&self, _root_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        Ok(Vec::new())
    }

    async fn update_thread_status(&self, _id: &ThreadId, _status: &str) -> Result<()> {
        Ok(())
    }

    async fn invalidate_context_cache(&self, _thread_id: &ThreadId) -> Result<()> {
        let mut invalidation_count = self.invalidation_count.lock().unwrap();
        *invalidation_count += 1;
        if self.fail_invalidation {
            anyhow::bail!(
                "deterministic injected error on cache invalidation {}",
                *invalidation_count
            );
        }
        Ok(())
    }

    async fn update_message_flags(
        &self,
        _message_id: &MessageId,
        _flags: &MessageFlags,
    ) -> Result<()> {
        let mut flag_count = self.flag_count.lock().unwrap();
        *flag_count += 1;
        if self.fail_flag_on.contains(&*flag_count) {
            anyhow::bail!(
                "deterministic injected error on flag update {}",
                *flag_count
            );
        }
        Ok(())
    }

    async fn delete_messages(
        &self,
        _thread_id: &ThreadId,
        _message_ids: &[MessageId],
    ) -> Result<()> {
        Ok(())
    }
}

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

// ── Compaction lifecycle 原子提交 ───────────────────────────────────────────

#[tokio::test]
async fn test_commit_compaction_lifecycle_sqlite_updates_memory_and_store_atomically() {
    let dir = tempdir().unwrap();
    let store = SqliteThreadStore::new(dir.path().join("transcript-lifecycle.db"))
        .await
        .unwrap();
    let thread_id = store.create_thread(ThreadMeta::new("/test")).await.unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(store);
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), thread_id.clone());

    let first_id = transcript.append(make_human("原始用户消息"));
    let second_id = transcript.append(make_ai("原始助手回复"));
    transcript.flush_persistence().await.unwrap();

    let summary = make_human("压缩摘要");
    let reinject = make_human("重新注入的用户上下文");
    let summary_id = summary.id();
    let reinject_id = reinject.id();
    transcript
        .commit_compaction_lifecycle(CompactionLifecycle {
            flag_updates: vec![
                (
                    first_id,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
                (
                    second_id,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
            ],
            appended_messages: vec![summary, reinject],
        })
        .await
        .unwrap();

    assert_eq!(transcript.entries().len(), 4);
    let visible = transcript.visible_messages();
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].id(), summary_id);
    assert_eq!(visible[1].id(), reinject_id);
    assert!(transcript.flags(first_id).excluded);
    assert!(transcript.flags(second_id).excluded);

    let messages = store.load_messages(&thread_id).await.unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].id(), first_id);
    assert_eq!(messages[1].id(), second_id);
    assert_eq!(messages[2].id(), summary_id);
    assert_eq!(messages[3].id(), reinject_id);
    let flags = store.load_message_flags(&thread_id).await.unwrap();
    assert!(flags[&first_id].excluded);
    assert!(flags[&second_id].excluded);
}

#[tokio::test]
async fn test_commit_compaction_lifecycle_waits_for_pending_sqlite_appends_in_fifo_order() {
    let dir = tempdir().unwrap();
    let store = SqliteThreadStore::new(dir.path().join("transcript-lifecycle-fifo.db"))
        .await
        .unwrap();
    let thread_id = store.create_thread(ThreadMeta::new("/test")).await.unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(store);
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), thread_id.clone());

    let first_id = transcript.append(make_human("FIFO 原始用户消息"));
    let second_id = transcript.append(make_ai("FIFO 原始助手回复"));

    let summary = make_human("FIFO 压缩摘要");
    let summary_id = summary.id();
    transcript
        .commit_compaction_lifecycle(CompactionLifecycle {
            flag_updates: vec![
                (
                    first_id,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
                (
                    second_id,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
            ],
            appended_messages: vec![summary],
        })
        .await
        .unwrap();

    let messages = store.load_messages(&thread_id).await.unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].id(), first_id);
    assert_eq!(messages[1].id(), second_id);
    assert_eq!(messages[2].id(), summary_id);
    let flags = store.load_message_flags(&thread_id).await.unwrap();
    assert!(flags[&first_id].excluded);
    assert!(flags[&second_id].excluded);

    assert_eq!(transcript.entries().len(), 3);
    assert!(transcript.flags(first_id).excluded);
    assert!(transcript.flags(second_id).excluded);
    let visible = transcript.visible_messages();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id(), summary_id);
}

#[tokio::test]
async fn test_commit_compaction_lifecycle_filesystem_failure_leaves_memory_and_store_unchanged() {
    let dir = tempdir().unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(FilesystemThreadStore::new(dir.path()));
    let thread_id = store.create_thread(ThreadMeta::new("/test")).await.unwrap();
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), thread_id.clone());

    let first_id = transcript.append(make_human("文件系统原始用户消息"));
    let second_id = transcript.append(make_ai("文件系统原始助手回复"));
    transcript.flush_persistence().await.unwrap();

    let summary = make_human("文件系统不应追加的摘要");
    let summary_id = summary.id();
    let error = transcript
        .commit_compaction_lifecycle(CompactionLifecycle {
            flag_updates: vec![
                (
                    first_id,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
                (
                    second_id,
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                ),
            ],
            appended_messages: vec![summary],
        })
        .await
        .unwrap_err();
    let error_message = error.to_string().to_lowercase();
    assert!(
        error_message.contains("filesystem") || error_message.contains("sqltiethreadstore"),
        "文件系统 store 必须明确拒绝 compact lifecycle: {error}"
    );

    assert_eq!(transcript.entries().len(), 2);
    let visible = transcript.visible_messages();
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].id(), first_id);
    assert_eq!(visible[1].id(), second_id);
    assert_eq!(transcript.flags(first_id), MessageFlags::default());
    assert_eq!(transcript.flags(second_id), MessageFlags::default());

    let messages = store.load_messages(&thread_id).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|message| message.id() != summary_id));
    let flags = store.load_message_flags(&thread_id).await.unwrap();
    assert!(flags.is_empty());
}

// ── 持久化 flush/barrier ───────────────────────────────────────────────────

#[tokio::test]
async fn test_message_transcript_flush_persistence_makes_appends_visible() {
    let store = Arc::new(FaultInjectingStore::new([]));
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "flush-visible".to_string());

    transcript.append(make_human("persisted message"));
    transcript.flush_persistence().await.unwrap();

    let messages = store.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content(), "persisted message");
}

#[tokio::test]
async fn test_message_transcript_flush_persistence_returns_error_once_and_recovers() {
    let store = Arc::new(FaultInjectingStore::new([2]));
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "flush-error".to_string());

    transcript.append(make_human("first"));
    transcript.append(make_human("second"));

    let error = transcript.flush_persistence().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deterministic injected error on append 2"),
        "flush 应返回 barrier 后首个写入错误: {error}"
    );
    assert_eq!(store.messages().len(), 1, "失败写入不应伪装为成功");

    transcript.flush_persistence().await.unwrap();
}

#[tokio::test]
async fn test_message_transcript_rebuild_keeps_persistence_writer_alive() {
    let store = Arc::new(FaultInjectingStore::new([]));
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "rebuild-writer".to_string());

    transcript.append(make_human("rebuild 前消息"));
    transcript.flush_persistence().await.unwrap();

    let entries: Vec<(BaseMessage, MessageFlags)> = transcript
        .entries()
        .iter()
        .map(|entry| (entry.message.clone(), transcript.flags(entry.message.id())))
        .collect();
    let mut rebuilt = transcript.rebuild(entries);

    rebuilt.append(make_human("rebuild 后消息"));
    rebuilt.flush_persistence().await.unwrap();

    let messages = store.messages();
    assert_eq!(messages.len(), 2, "rebuild 后追加的消息必须持久化");
    assert_eq!(messages[1].content(), "rebuild 后消息");
}

#[tokio::test]
async fn test_message_transcript_flush_persistence_returns_first_of_multiple_errors() {
    let store = Arc::new(FaultInjectingStore::new([2, 3]));
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "multiple-errors".to_string());

    transcript.append(make_human("first"));
    transcript.append(make_human("second"));
    transcript.append(make_human("third"));

    let error = transcript.flush_persistence().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deterministic injected error on append 2"),
        "同一 barrier 前的多个写入失败必须返回第一个错误: {error}"
    );
    assert_eq!(store.messages().len(), 1, "两个失败写入都不应伪装为成功");

    transcript.flush_persistence().await.unwrap();
}

#[tokio::test]
async fn test_apply_compaction_batch_returns_first_flag_error_and_invalidates_cache() {
    let store = Arc::new(FaultInjectingStore::with_failures([], [1, 2], false));
    let transcript = MessageTranscript::new()
        .with_persistence(store.clone(), "batch-first-flag-error".to_string());

    transcript.send_persist(PersistOp::ApplyCompactionBatch {
        updates: vec![
            (MessageId::new(), MessageFlags::default()),
            (MessageId::new(), MessageFlags::default()),
        ],
    });

    let error = transcript.flush_persistence().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deterministic injected error on flag update 1"),
        "batch 必须向 barrier 暴露第一个 flag 更新失败: {error}"
    );
    assert_eq!(*store.flag_count.lock().unwrap(), 2, "后续更新仍应执行");
    assert_eq!(
        *store.invalidation_count.lock().unwrap(),
        1,
        "batch 必须只失效一次缓存"
    );
}

#[tokio::test]
async fn test_apply_compaction_batch_returns_cache_invalidation_error() {
    let store = Arc::new(FaultInjectingStore::with_failures([], [], true));
    let transcript =
        MessageTranscript::new().with_persistence(store, "batch-invalidation-error".to_string());

    transcript.send_persist(PersistOp::ApplyCompactionBatch {
        updates: vec![(MessageId::new(), MessageFlags::default())],
    });

    let error = transcript.flush_persistence().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deterministic injected error on cache invalidation 1"),
        "cache invalidation 失败必须向 barrier 暴露: {error}"
    );
}

#[tokio::test]
async fn test_message_transcript_flush_persistence_without_backend_is_ok() {
    MessageTranscript::new().flush_persistence().await.unwrap();
}

// ── Drop 优雅关闭（issue 2026-08-05-transcript-drop-loses-final-messages）─────────
//
// Drop 不得 abort writer：abort 会立即取消任务，pending_appends 和通道中未处理的
// 消息被直接丢弃。改为发送 Shutdown 信号，writer flush 剩余积压后自行退出。
// writer 是 detached task（持有 store 的独立 Arc），测试用轮询验证最终落库。

#[tokio::test]
async fn test_drop_flushes_pending_appends_to_store() {
    // 不调用 flush_persistence 直接 drop：模拟 turn 正常结束，最终回答落在
    // 100ms 批量窗口内未落库的场景。drop 后最后一批必须仍全部落库。
    let store = Arc::new(FaultInjectingStore::new([]));
    {
        let mut transcript =
            MessageTranscript::new().with_persistence(store.clone(), "drop-flush".to_string());
        transcript.append(make_human("final-answer-1"));
        transcript.append(make_human("final-answer-2"));
    }

    // writer detached：轮询 store 直到最后一批落库（带超时防挂死）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.messages().len() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("drop 后 writer 应在超时前 flush 剩余消息");

    let messages = store.messages();
    assert_eq!(messages.len(), 2, "drop 后最后一批消息必须全部落库");
    assert_eq!(
        messages[0].content(),
        "final-answer-1",
        "落库顺序必须与 append 顺序一致"
    );
    assert_eq!(
        messages[1].content(),
        "final-answer-2",
        "落库顺序必须与 append 顺序一致"
    );
}

#[tokio::test]
async fn test_drop_without_persistence_is_noop() {
    // 无持久化绑定时 drop 不应 panic / 不应有副作用
    drop(MessageTranscript::new());
}

#[tokio::test]
async fn test_shutdown_persistence_flushes_pending_and_writer_exits() {
    let store = Arc::new(FaultInjectingStore::new([]));
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "shutdown-flush".to_string());
    transcript.append(make_human("last batch before shutdown"));

    transcript.shutdown_persistence();

    // writer 收到 Shutdown 后 flush 剩余并退出；退出后通道关闭，
    // flush_persistence 应返回错误（channel closed）——轮询等待该状态
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(()) = transcript.flush_persistence().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown 后 writer 应在超时前 flush 并退出");

    let messages = store.messages();
    assert_eq!(messages.len(), 1, "shutdown 前积压的消息必须落库");
    assert_eq!(messages[0].content(), "last batch before shutdown");
}

// ── 基础构造 ──────────────────────────────────────────────────────────────

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
    assert_eq!(t.get(id).unwrap().message.id(), id);
}

#[test]
fn test_append_batch() {
    let mut t = MessageTranscript::new();
    let msgs = vec![make_human("a"), make_human("b"), make_human("c")];
    let ids = t.append_batch(msgs);

    assert_eq!(ids.len(), 3);
    assert_eq!(t.len(), 3);
    // 按 append 顺序存储
    assert_eq!(t.entries()[0].message.content(), "a");
    assert_eq!(t.entries()[1].message.content(), "b");
    assert_eq!(t.entries()[2].message.content(), "c");
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
    assert_eq!(t.entries()[1].message.content(), "thinking...");
    assert_eq!(t.entries()[2].message.content(), "result-1");
    assert_eq!(t.entries()[3].message.content(), "result-2");
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
    assert_eq!(t.entries()[0].message.content(), "second ai");
    assert_eq!(t.entries()[1].message.content(), "result for second");
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
            t.entries()[0].message.clone(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        ),
        (t.entries()[1].message.clone(), MessageFlags::default()),
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
        .map(|e| (e.message.clone(), MessageFlags::default()))
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

    let entries = vec![(t.entries()[0].message.clone(), MessageFlags::default())];
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
            let fid = e.message.id();
            let mut flags = t.flags(fid);
            if fid == id {
                flags.projection = Some(directive.clone());
            }
            (e.message.clone(), flags)
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
