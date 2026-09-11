//! Compact 提交后跨 turn 的恢复回归；复用生产 executor/host 收尾与 SQLite。

use super::*;
use crate::host::{prompt::finish_prompt_turn, SessionState, SharedSessions};
use peri_acp_types::{
    messages::MessageId,
    store::{CompactionLifecycle, MessageFlags, PersistedPayload},
    thread::{ThreadId, ThreadMeta},
};
use peri_agent::thread::SqliteThreadStore;
use std::{collections::HashMap, sync::atomic::AtomicBool};

const SUMMARY: &str = "COMMITTED_COMPACT_RECOVERY_SUMMARY";
const OLD: &str = "OLD_HISTORY_MUST_STAY_EXCLUDED";

struct SummaryModel;

#[async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        let response = ModelResponse::new(
            ModelMessage::assistant_text(SUMMARY),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

enum AfterCommitAction {
    Cancel(AgentCancellationToken),
    Error,
}

// 故障注入均包在真实 SQLite 调用外围，不能用内存替身伪造提交。
struct RecoveryStore {
    inner: SqliteThreadStore,
    compact_commits: AtomicUsize,
    fail_appends: AtomicBool,
    fail_after_full: bool,
    deletes: AtomicUsize,
    after_commit: Mutex<Option<AfterCommitAction>>,
}

#[async_trait]
impl ThreadStore for RecoveryStore {
    async fn create_thread(&self, meta: ThreadMeta) -> anyhow::Result<ThreadId> {
        self.inner.create_thread(meta).await
    }
    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> anyhow::Result<()> {
        self.append_payloads(
            id,
            &msgs
                .iter()
                .cloned()
                .map(PersistedPayload::Message)
                .collect::<Vec<_>>(),
        )
        .await
    }
    async fn append_payloads(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> anyhow::Result<()> {
        if self.fail_appends.load(Ordering::SeqCst)
            || (self.fail_after_full && self.compact_commits.load(Ordering::SeqCst) > 0)
        {
            anyhow::bail!("injected post-compact writer failure");
        }
        self.inner.append_payloads(id, payloads).await
    }
    async fn load_messages(&self, id: &ThreadId) -> anyhow::Result<Vec<BaseMessage>> {
        self.inner.load_messages(id).await
    }
    async fn load_payloads(&self, id: &ThreadId) -> anyhow::Result<Vec<PersistedPayload>> {
        self.inner.load_payloads(id).await
    }
    async fn load_meta(&self, id: &ThreadId) -> anyhow::Result<ThreadMeta> {
        self.inner.load_meta(id).await
    }
    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> anyhow::Result<()> {
        self.inner.update_meta(id, meta).await
    }
    async fn list_threads(&self) -> anyhow::Result<Vec<ThreadMeta>> {
        self.inner.list_threads().await
    }
    async fn delete_thread(&self, id: &ThreadId) -> anyhow::Result<()> {
        self.inner.delete_thread(id).await
    }
    async fn load_context(&self, id: &ThreadId) -> anyhow::Result<Vec<BaseMessage>> {
        self.inner.load_context(id).await
    }
    async fn load_context_payloads(&self, id: &ThreadId) -> anyhow::Result<Vec<PersistedPayload>> {
        self.inner.load_context_payloads(id).await
    }
    async fn list_child_threads(&self, id: &ThreadId) -> anyhow::Result<Vec<ThreadMeta>> {
        self.inner.list_child_threads(id).await
    }
    async fn list_session_threads(&self, id: &ThreadId) -> anyhow::Result<Vec<ThreadMeta>> {
        self.inner.list_session_threads(id).await
    }
    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> anyhow::Result<()> {
        self.inner.update_thread_status(id, status).await
    }
    async fn invalidate_context_cache(&self, id: &ThreadId) -> anyhow::Result<()> {
        self.inner.invalidate_context_cache(id).await
    }
    async fn delete_messages(&self, id: &ThreadId, ids: &[MessageId]) -> anyhow::Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete_messages(id, ids).await
    }
    async fn update_message_flags(
        &self,
        id: &MessageId,
        flags: &MessageFlags,
    ) -> anyhow::Result<()> {
        self.inner.update_message_flags(id, flags).await
    }
    fn supports_compaction_lifecycle(&self) -> bool {
        true
    }
    async fn commit_compaction_lifecycle(
        &self,
        id: &ThreadId,
        lifecycle: &CompactionLifecycle,
    ) -> anyhow::Result<()> {
        self.inner
            .commit_compaction_lifecycle(id, lifecycle)
            .await?;
        if !lifecycle.appended_messages.is_empty() {
            self.compact_commits.fetch_add(1, Ordering::SeqCst);
            let action = self.after_commit.lock().unwrap().take();
            match action {
                Some(AfterCommitAction::Cancel(cancel)) => {
                    cancel.cancel();
                    std::future::pending::<()>().await;
                }
                Some(AfterCommitAction::Error) => {
                    anyhow::bail!("injected error after durable compact commit");
                }
                None => {}
            }
        }
        Ok(())
    }
    async fn load_message_flags(
        &self,
        id: &ThreadId,
    ) -> anyhow::Result<HashMap<MessageId, MessageFlags>> {
        self.inner.load_message_flags(id).await
    }
}

fn make_host_sessions(ctx: &SessionContext, payloads: Vec<PersistedPayload>) -> SharedSessions {
    let state = SessionState {
        session_id: ctx.session_id.clone(),
        thread_id: ctx.thread_id.clone().unwrap(),
        cwd: ctx.cwd.clone(),
        history: payloads
            .iter()
            .filter_map(|payload| payload.as_message().cloned())
            .collect(),
        history_payloads: payloads,
        cancel_token: Some(ctx.cancel.clone()),
        frozen: Some(make_sentinel_frozen()),
        recall_items: vec![],
        agent_pool: AgentPool::new(),
        workflow_middleware: None,
        lsp_pool: None,
        title: None,
        tags: vec![],
        continuation_armed: false,
        continuation_epoch: 0,
        continuation_in_flight: false,
        continuation_mq_steering_pending: false,
        lease: crate::host::lease::WriterLease::acquired("default"),
    };
    Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        ctx.session_id.clone(),
        state,
    )])))
}

async fn make_recovery_context(
    dir: &tempfile::TempDir,
    model: Arc<dyn Model>,
    fail_after_full: bool,
) -> (SessionContext, Arc<RecoveryStore>, SharedSessions) {
    let store = Arc::new(RecoveryStore {
        inner: SqliteThreadStore::new(dir.path().join("recovery.db"))
            .await
            .unwrap(),
        compact_commits: AtomicUsize::new(0),
        fail_appends: AtomicBool::new(false),
        fail_after_full,
        deletes: AtomicUsize::new(0),
        after_commit: Mutex::new(None),
    });
    let cwd = dir.path().to_str().unwrap();
    let thread_id = store.create_thread(ThreadMeta::new(cwd)).await.unwrap();
    let history = vec![BaseMessage::human(OLD), BaseMessage::ai("old answer")];
    store.append_messages(&thread_id, &history).await.unwrap();
    let mut ctx = make_session_context(&thread_id);
    ctx.cwd = cwd.into();
    ctx.thread_id = Some(thread_id);
    ctx.thread_store = Some(store.clone());
    ctx.primary_llm_factory = Some(Arc::new(move || model.clone()));
    let sessions = make_host_sessions(
        &ctx,
        history.into_iter().map(PersistedPayload::Message).collect(),
    );
    (ctx, store, sessions)
}

async fn make_recovery_turn(
    ctx: &SessionContext,
    sessions: &SharedSessions,
    trigger_full: bool,
) -> TurnInput {
    let stage = make_stage_build(ctx);
    let stage_build: StageBuildFn = Arc::new(move |request| {
        let (mut output, cache) = stage(request)?;
        if trigger_full {
            // 模拟上一 Reason 的有效高压 usage，Full 本身走真实 Compact stage/事务。
            output
                .context
                .compact
                .token_tracker
                .write()
                .accumulate(&TokenUsage {
                    input_tokens: 196_000,
                    output_tokens: 1,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                });
            output.context.compact.compact_llm = Some(Arc::new(SummaryModel));
        }
        Ok((output, cache))
    });
    let sessions = sessions.lock().await;
    let state = sessions.get(&ctx.session_id).unwrap();
    let mut turn = make_turn_input(
        Arc::new(MockEventSink::new()),
        MessageContent::text("continue recovery lifecycle"),
        false,
        state.history.clone(),
        stage_build,
    );
    turn.history_payloads = state.history_payloads.clone();
    turn.frozen = state.frozen.clone();
    turn
}

async fn assert_next_turn_sees_summary(mut ctx: SessionContext, sessions: &SharedSessions) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: requests.clone(),
    });
    ctx.primary_llm_factory = Some(Arc::new(move || model.clone()));
    ctx.cancel = AgentCancellationToken::new();
    let turn = make_recovery_turn(&ctx, sessions, false).await;
    let result = run_session_loop(ctx, turn).await;
    assert!(result.ok, "恢复后的下一轮应成功: {:?}", result.failure);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "下一轮必须真正到达模型");
    let text = requests[0]
        .messages
        .iter()
        .filter_map(ModelMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(SUMMARY), "下一轮必须包含已提交摘要");
    assert!(!text.contains(OLD), "旧历史 excluded 标记必须继续生效");
}

/// [回归测试] Full 提交后 cancel 的结果仍更新热 host，下一轮恢复摘要与 flags。
#[tokio::test]
#[serial]
async fn test_full_compact_cancel_preserves_next_turn_summary() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let model: Arc<dyn Model> = Arc::new(CancelGateModel {
        entered: Mutex::new(Some(entered_tx)),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let running_ctx = ctx.clone();
    let task = tokio::spawn(async move { run_session_loop(running_ctx, turn).await });
    entered_rx.await.unwrap();
    assert_eq!(
        store.compact_commits.load(Ordering::SeqCst),
        1,
        "取消必须发生在真实 Full 提交之后"
    );
    ctx.cancel.cancel();
    let result = task.await.unwrap();
    assert!(!result.ok);
    assert!(result.history_replaced_by_compaction);
    assert!(!result.persistence_inconsistent);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap();
    assert_eq!(wire["stopReason"], "cancelled");
    assert!(sessions.lock().await[&ctx.session_id]
        .cancel_token
        .is_none());
    assert_next_turn_sees_summary(ctx, &sessions).await;
}

/// [回归测试] Full 提交后的 fatal LLM error 保持 wire error，同时采纳 canonical progress。
#[tokio::test]
#[serial]
async fn test_full_compact_llm_error_preserves_next_turn_summary() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_recovery_context(&dir, Arc::new(FatalModel), false).await;
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 1);
    assert!(!result.ok);
    assert!(result.history_replaced_by_compaction);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(
        wire.code,
        crate::host::prompt::ACP_TURN_EXECUTION_FAILED_CODE
    );
    assert_eq!(wire.data.unwrap()["kind"], "llm_http");
    assert_next_turn_sees_summary(ctx, &sessions).await;
}

/// [回归测试] Full 提交后的 forwarder 失败不丢 canonical 摘要。
#[tokio::test]
#[serial]
async fn test_full_compact_forwarder_error_preserves_next_turn_summary() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    let mut turn = make_recovery_turn(&ctx, &sessions, true).await;
    turn.forwarder_launcher = make_aborting_forwarder_launcher();
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 1);
    assert!(!result.ok);
    assert!(result.history_replaced_by_compaction);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert_next_turn_sees_summary(ctx, &sessions).await;
}

/// [回归测试] Full 事务后追加失败，不能按 turn ID 删除摘要；新 SQLite owner 可恢复。
#[tokio::test]
#[serial]
async fn test_full_compact_writer_error_evicts_host_and_cold_reload_recovers_summary() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let (mut ctx, store, sessions) = make_recovery_context(&dir, model, true).await;
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 1);
    assert!(!result.ok);
    assert!(
        result.persistence_inconsistent,
        "写失败必须使 host snapshot 失效"
    );
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert!(!sessions.lock().await.contains_key(&ctx.session_id));
    assert_eq!(
        store.deletes.load(Ordering::SeqCst),
        0,
        "不得删除已提交 lifecycle 的消息"
    );
    // 释放原执行与持久化 owner，再打开全新的 SQLite store。
    ctx.thread_store = None;
    drop(store);
    let recovered = Arc::new(
        SqliteThreadStore::new(dir.path().join("recovery.db"))
            .await
            .unwrap(),
    );
    let thread_id = ctx.thread_id.as_ref().unwrap();
    let payloads = recovered.load_payloads(thread_id).await.unwrap();
    let flags = recovered.load_message_flags(thread_id).await.unwrap();
    let old = payloads
        .iter()
        .find(|p| p.as_message().is_some_and(|m| m.content() == OLD))
        .unwrap();
    assert!(flags[&old.id()].excluded);
    assert_eq!(
        payloads
            .iter()
            .filter(|p| p
                .as_message()
                .is_some_and(|m| m.content().contains(SUMMARY)))
            .count(),
        1
    );
    ctx.thread_store = Some(recovered);
    let cold_sessions = make_host_sessions(&ctx, payloads);
    assert_next_turn_sees_summary(ctx, &cold_sessions).await;
}

/// [回归测试] 未发生 Full 的 writer 错误同样移除热状态，不能假称 ID 回滚成功。
#[tokio::test]
#[serial]
async fn test_writer_error_without_compact_evicts_host_without_deleting_durable_history() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    store.fail_appends.store(true, Ordering::SeqCst);
    let turn = make_recovery_turn(&ctx, &sessions, false).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(!result.ok);
    assert!(result.persistence_inconsistent);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert!(!sessions.lock().await.contains_key(&ctx.session_id));
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 0);
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .load_payloads(ctx.thread_id.as_ref().unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
}

/// [回归测试] 未执行的真实 PromptHandle 不提供可验证 snapshot，host 必须要求冷加载。
#[tokio::test]
#[serial]
async fn test_missing_prompt_result_evicts_host_without_adopting_empty_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_recovery_context(&dir, Arc::new(FatalModel), false).await;
    let turn = make_recovery_turn(&ctx, &sessions, false).await;
    let handle = crate::host::prompt_handle::PromptHandle::new(ctx.clone(), turn);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, handle.take_result())
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert!(!sessions.lock().await.contains_key(&ctx.session_id));
    assert_eq!(
        store
            .load_payloads(ctx.thread_id.as_ref().unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
}

/// [回归测试] 装配失败仍回传原 canonical snapshot，可以保留热状态；区别于缺失结果。
#[tokio::test]
#[serial]
async fn test_stage_initialization_failure_preserves_verified_previous_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_recovery_context(&dir, Arc::new(FatalModel), false).await;
    let mut turn = make_recovery_turn(&ctx, &sessions, false).await;
    let previous_ids = turn
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect::<Vec<_>>();
    turn.stage_build = Arc::new(|_| {
        Err(
            peri_agent::session::exec::stage_builder::StageBuildError::ToolCatalog(
                peri_agent::session::tool_catalog::CatalogRefreshError::AliasConflict,
            ),
        )
    });
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(!result.ok);
    assert!(!result.persistence_inconsistent);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    let sessions = sessions.lock().await;
    let state = sessions
        .get(&ctx.session_id)
        .expect("装配前未修改持久化，热状态仍有效");
    assert_eq!(
        state
            .history_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        previous_ids
    );
    assert!(state.cancel_token.is_none());
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
}

async fn assert_unconfirmed_commit_requires_cold_recovery(cancel_after_commit: bool) {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: requests.clone(),
    });
    let (mut ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    *store.after_commit.lock().unwrap() = Some(if cancel_after_commit {
        AfterCommitAction::Cancel(ctx.cancel.clone())
    } else {
        AfterCommitAction::Error
    });
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(
        store.compact_commits.load(Ordering::SeqCst),
        1,
        "故障必须发生在真实COMMIT之后"
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "未知持久化状态不能继续Reason读取旧快照"
    );
    assert!(!result.ok);
    assert!(
        result.persistence_inconsistent,
        "普通writer barrier成功不能抹去未确认commit"
    );
    assert_eq!(
        result.failure.as_ref().unwrap().kind,
        peri_acp_types::session::ExecutionFailureKind::Internal
    );
    assert_eq!(
        result.stop_reason,
        PromptStopReason::EndTurn,
        "未知持久化沿用Internal收尾，不能伪装普通cancel"
    );
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert!(wire.message.contains("reload"));
    assert!(!sessions.lock().await.contains_key(&ctx.session_id));
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
    ctx.thread_store = None;
    drop(store);
    let recovered = Arc::new(
        SqliteThreadStore::new(dir.path().join("recovery.db"))
            .await
            .unwrap(),
    );
    let thread_id = ctx.thread_id.as_ref().unwrap();
    let payloads = recovered.load_payloads(thread_id).await.unwrap();
    let flags = recovered.load_message_flags(thread_id).await.unwrap();
    let old = payloads
        .iter()
        .find(|p| p.as_message().is_some_and(|m| m.content() == OLD))
        .unwrap();
    assert!(flags[&old.id()].excluded);
    assert_eq!(
        payloads
            .iter()
            .filter(|p| p
                .as_message()
                .is_some_and(|m| m.content().contains(SUMMARY)))
            .count(),
        1
    );
    ctx.thread_store = Some(recovered);
    let cold_sessions = make_host_sessions(&ctx, payloads);
    assert_next_turn_sees_summary(ctx, &cold_sessions).await;
}

/// [回归测试] SQLite已COMMIT但尚未apply内存时取消，不能把旧snapshot当成功flush结果。
#[tokio::test]
#[serial]
async fn test_full_compact_cancel_after_durable_commit_requires_cold_recovery() {
    assert_unconfirmed_commit_requires_cold_recovery(true).await;
}

/// [回归测试] store已COMMIT后返回Err，不能降级继续Reason或采纳旧snapshot。
#[tokio::test]
#[serial]
async fn test_full_compact_error_after_durable_commit_requires_cold_recovery() {
    assert_unconfirmed_commit_requires_cold_recovery(false).await;
}

/// [回归测试] Full内部barrier消费写错误后，Phase8的成功barrier仍不得证明快照安全。
#[tokio::test]
#[serial]
async fn test_full_compact_precommit_flush_error_remains_uncertain_at_final_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: requests.clone(),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    store.fail_appends.store(true, Ordering::SeqCst);
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(result.persistence_inconsistent);
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 0);
    assert!(requests.lock().unwrap().is_empty());
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert!(!sessions.lock().await.contains_key(&ctx.session_id));
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .load_payloads(ctx.thread_id.as_ref().unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
}
