//! Manual compact must preserve the SQLite commit outcome when either select drops its future.
use super::*;
use crate::session::exec::compact_pipeline::execute_compact;
use crate::thread::{SqliteThreadStore, ThreadId, ThreadMeta};
use peri_acp_types::messages::MessageId;
use peri_acp_types::store::{
    CompactionLifecycle, InheritedContext, MessageFlags, PersistedPayload, ThreadStore,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy)]
enum CommitMode {
    Normal,
    CancelBefore,
    CancelAfter,
    ErrorAfter,
}
#[derive(Clone, Copy)]
enum HandlerPause {
    None,
    AfterPipeline,
    FailReload,
}

struct ControlledStore {
    inner: SqliteThreadStore,
    mode: CommitMode,
    cancel: AgentCancellationToken,
    fail_reload: AtomicBool,
    calls: AtomicUsize,
}
#[async_trait]
impl ThreadStore for ControlledStore {
    async fn create_thread(&self, meta: ThreadMeta) -> anyhow::Result<ThreadId> {
        self.inner.create_thread(meta).await
    }
    async fn append_messages(&self, id: &ThreadId, messages: &[BaseMessage]) -> anyhow::Result<()> {
        self.inner.append_messages(id, messages).await
    }
    async fn load_messages(&self, id: &ThreadId) -> anyhow::Result<Vec<BaseMessage>> {
        self.inner.load_messages(id).await
    }
    async fn load_payloads(&self, id: &ThreadId) -> anyhow::Result<Vec<PersistedPayload>> {
        if self.fail_reload.load(Ordering::SeqCst) {
            anyhow::bail!("injected canonical reload failure");
        }
        self.inner.load_payloads(id).await
    }
    async fn load_inherited_context(&self, id: &ThreadId) -> anyhow::Result<InheritedContext> {
        self.inner.load_inherited_context(id).await
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
        self.inner.delete_messages(id, ids).await
    }
    async fn load_message_flags(
        &self,
        id: &ThreadId,
    ) -> anyhow::Result<HashMap<MessageId, MessageFlags>> {
        self.inner.load_message_flags(id).await
    }
    fn supports_compaction_lifecycle(&self) -> bool {
        true
    }
    async fn commit_compaction_lifecycle(
        &self,
        id: &ThreadId,
        lifecycle: &CompactionLifecycle,
    ) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.mode, CommitMode::CancelBefore) {
            self.cancel.cancel();
            return std::future::pending().await;
        }
        self.inner
            .commit_compaction_lifecycle(id, lifecycle)
            .await?;
        match self.mode {
            CommitMode::CancelAfter => {
                self.cancel.cancel();
                std::future::pending().await
            }
            CommitMode::ErrorAfter => anyhow::bail!("injected lost COMMIT acknowledgment"),
            _ => Ok(()),
        }
    }
}

struct SummaryModel;
#[async_trait]
impl peri_model::Model for SummaryModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        Default::default()
    }
    async fn stream(
        &self,
        _: peri_model::ModelRequest,
        _: AgentCancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        Err(peri_model::ModelError::cancelled())
    }
    async fn complete(
        &self,
        _: peri_model::ModelRequest,
        _: AgentCancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelResponse> {
        peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text("<summary>manual committed summary</summary>"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

struct PipelineHandler {
    pause: HandlerPause,
    store: Arc<ControlledStore>,
}
#[async_trait]
impl CommandHandler for PipelineHandler {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        let cancel = ctx.cancel_token.clone();
        let result = execute_compact(ctx).await;
        if !matches!(self.pause, HandlerPause::None) {
            self.store.fail_reload.store(
                matches!(self.pause, HandlerPause::FailReload),
                Ordering::SeqCst,
            );
            cancel.cancel();
            return std::future::pending().await;
        }
        CommandOutcome::Done(result)
    }
}

struct Case {
    _dir: tempfile::TempDir,
    store: Arc<ControlledStore>,
    thread_id: ThreadId,
    history: Vec<BaseMessage>,
    result: peri_acp_types::session::PromptResult,
    done_count: usize,
}
async fn run_case(mode: CommitMode, pause: HandlerPause, pre_cancel: bool) -> Case {
    let dir = tempfile::tempdir().unwrap();
    let cancel = AgentCancellationToken::new();
    let store = Arc::new(ControlledStore {
        inner: SqliteThreadStore::new(dir.path().join("manual.db"))
            .await
            .unwrap(),
        mode,
        cancel: cancel.clone(),
        fail_reload: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_str().unwrap()))
        .await
        .unwrap();
    let history = vec![
        BaseMessage::human("old manual question"),
        BaseMessage::ai("old manual answer"),
    ];
    store.append_messages(&thread_id, &history).await.unwrap();
    if pre_cancel {
        cancel.cancel();
    }
    let handler: Arc<dyn CommandHandler> = Arc::new(PipelineHandler {
        pause,
        store: store.clone(),
    });
    let lookup: super::super::CommandLookupFn = Arc::new(move |_| {
        let mut entry = test_route_entry();
        entry.handler = handler.clone();
        Some(ResolvedCommand {
            entry: Arc::new(entry),
            args: String::new(),
        })
    });
    let content = MessageContent::text("/compact");
    let sink = Arc::new(MockEventSink::new());
    let event_sink: Arc<dyn EventSink> = sink.clone();
    let (bg_tx, task_manager) = make_bg_infra();
    let model: Option<Arc<dyn peri_model::Model>> = Some(Arc::new(SummaryModel));
    let mut req = make_intercept_request(
        &content,
        &history,
        "manual",
        &cancel,
        &event_sink,
        &bg_tx,
        &task_manager,
        lookup,
    );
    req.cwd = dir.path().to_str().unwrap();
    req.thread_store = Some(store.clone());
    req.thread_id = Some(thread_id.clone());
    req.auxiliary_model = &model;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        intercept_immediate_command(req),
    )
    .await
    .expect("manual cancellation must terminate");
    let InterceptOutcome::Handled(result) = outcome else {
        panic!("manual command must be handled");
    };
    Case {
        _dir: dir,
        store,
        thread_id,
        history,
        result,
        done_count: sink.push_done_count(),
    }
}

async fn assert_durable_summary(case: &Case) {
    let payloads = case
        .store
        .inner
        .load_payloads(&case.thread_id)
        .await
        .unwrap();
    assert!(payloads.iter().any(|payload| payload
        .as_message()
        .is_some_and(|message| message.content().contains("manual committed summary"))));
    let flags = case
        .store
        .inner
        .load_message_flags(&case.thread_id)
        .await
        .unwrap();
    assert!(case
        .history
        .iter()
        .all(|message| flags[&message.id()].excluded));
    assert_eq!(case.done_count, 1);
}

#[tokio::test]
async fn test_manual_compact_cancel_before_commit_requires_reload_without_deleting_history() {
    let case = run_case(CommitMode::CancelBefore, HandlerPause::None, false).await;
    assert!(case.result.persistence_inconsistent);
    assert!(!case.result.ok);
    assert!(case.result.failure.is_some());
    assert_eq!(
        case.store
            .inner
            .load_messages(&case.thread_id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(case
        .store
        .inner
        .load_message_flags(&case.thread_id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(case.done_count, 1);
}

#[tokio::test]
async fn test_manual_compact_cancel_after_sql_commit_requires_reload_and_keeps_summary() {
    let case = run_case(CommitMode::CancelAfter, HandlerPause::None, false).await;
    assert!(case.result.persistence_inconsistent);
    assert!(!case.result.ok);
    assert!(case.result.failure.is_some());
    assert_durable_summary(&case).await;
}

#[tokio::test]
async fn test_manual_compact_error_after_sql_commit_requires_reload_and_keeps_summary() {
    let case = run_case(CommitMode::ErrorAfter, HandlerPause::None, false).await;
    assert!(case.result.persistence_inconsistent);
    assert!(case.result.failure.is_some());
    assert_durable_summary(&case).await;
}

#[tokio::test]
async fn test_manual_compact_cancel_after_confirmed_pipeline_restores_canonical_payloads() {
    let case = run_case(CommitMode::Normal, HandlerPause::AfterPipeline, false).await;
    assert!(!case.result.persistence_inconsistent);
    assert!(case.result.history_replaced_by_compaction);
    assert!(case.result.failure.is_none());
    assert_eq!(case.result.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(case.result.messages.len(), 1);
    assert!(case.result.messages[0]
        .content()
        .contains("manual committed summary"));
    let stored = case
        .store
        .inner
        .load_payloads(&case.thread_id)
        .await
        .unwrap();
    assert_eq!(
        case.result
            .persisted_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        stored.iter().map(PersistedPayload::id).collect::<Vec<_>>()
    );
    assert_durable_summary(&case).await;
}

#[tokio::test]
async fn test_manual_compact_confirmed_commit_reload_error_fails_closed() {
    let case = run_case(CommitMode::Normal, HandlerPause::FailReload, false).await;
    assert!(case.result.persistence_inconsistent);
    assert!(!case.result.history_replaced_by_compaction);
    assert!(case.result.failure.is_some());
    assert_durable_summary(&case).await;
}

#[tokio::test]
async fn test_manual_compact_precancel_preserves_verified_history_without_store_commit() {
    let case = run_case(CommitMode::Normal, HandlerPause::None, true).await;
    assert!(!case.result.persistence_inconsistent);
    assert!(!case.result.history_replaced_by_compaction);
    assert!(case.result.failure.is_none());
    assert_eq!(case.result.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(case.store.calls.load(Ordering::SeqCst), 0);
    assert_eq!(case.result.messages.len(), 2);
    assert_eq!(case.done_count, 1);
}
