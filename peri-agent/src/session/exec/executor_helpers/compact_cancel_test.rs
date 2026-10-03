//! Manual compact must preserve the SQLite commit outcome when either select drops its future.
use super::*;
use crate::session::exec::compact_pipeline::execute_compact;
use crate::session::test_resources::git_repository;
use crate::thread::{ThreadId, ThreadMeta};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, ForkSnapshot,
    FrozenSnapshotBytes, NewSession, NewSessionMeta, PersistenceRecovery, RewindBoundary,
    SessionAvailability, SessionMetaPatch, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult, SessionResources, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, PersistedPayload};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding, SessionExecutionLease,
    SESSION_BINDING_VERSION,
};
use peri_resources::sessions::SessionResourcesImpl;
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

/// 真门面 + 提交点注入：除了 `apply_compaction` 与快照重载，其余行为逐项转发。
///
/// 转发而不是重实现：包装层只注入「提交点被取消 / 提交后确认丢失 / 重载失败」三种
/// 时序，存储语义仍由真实实现提供。
struct ControlledStore {
    inner: Arc<dyn SessionResources>,
    mode: CommitMode,
    cancel: AgentCancellationToken,
    fail_reload: AtomicBool,
    calls: AtomicUsize,
}

#[async_trait]
impl SessionResources for ControlledStore {
    async fn inspect_availability(
        &self,
        session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        self.inner.inspect_availability(session).await
    }

    async fn resolve_workspace(
        &self,
        cwd: &std::path::Path,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        self.inner.resolve_workspace(cwd).await
    }

    async fn validate_session(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.inner.validate_session(id, workspace).await
    }

    async fn acquire_execution(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.inner.acquire_execution(id, workspace).await
    }

    async fn create_session(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.inner.create_session(input).await
    }

    async fn abandon_initialization(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        self.inner.abandon_initialization(id, lease).await
    }

    async fn begin_initialization(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn peri_acp_types::session_resources::SessionInitialization>>
    {
        self.inner.begin_initialization(draft).await
    }

    async fn discard_incomplete_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.discard_incomplete_initialization(id).await
    }

    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.inner
            .adopt_legacy_session(id, saved_cwd, workspace, frozen)
            .await
    }

    async fn load_session_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        if self.fail_reload.load(Ordering::SeqCst) {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Unavailable {
                    detail: "injected canonical reload failure".to_owned(),
                },
            ));
        }
        self.inner.load_session_snapshot(id).await
    }

    async fn load_session_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        self.inner.load_session_binding(id).await
    }

    async fn validate_bound_workspace(
        &self,
        id: &ThreadId,
        check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        self.inner.validate_bound_workspace(id, check).await
    }

    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        self.inner.load_session_history(id).await
    }

    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.inner.load_session_meta(id).await
    }

    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.inner.list_sessions(query).await
    }

    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.inner.list_children(parent).await
    }

    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.inner.list_session_tree(root).await
    }

    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.inner.append_history(id, payloads).await
    }

    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: peri_acp_types::messages::MessageId,
        reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.inner
            .append_reminder_if_absent(id, message_id, reminder)
            .await
    }

    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.mark_session_closing(id).await
    }

    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.inner.is_session_closing(id).await
    }

    async fn save_fork(
        &self,
        fork: &ForkSnapshot,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.inner.save_fork(fork).await
    }

    async fn save_child(
        &self,
        child: &ChildSnapshot,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        self.inner.save_child(child, lease).await
    }

    async fn claim_child_resume(
        &self,
        child: &ThreadId,
        root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        self.inner.claim_child_resume(child, root).await
    }

    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.mode, CommitMode::CancelBefore) {
            self.cancel.cancel();
            return std::future::pending().await;
        }
        let result = self.inner.apply_compaction(id, change).await;
        match self.mode {
            CommitMode::CancelAfter => {
                self.cancel.cancel();
                std::future::pending().await
            }
            CommitMode::ErrorAfter => Err(SessionResourceError::new(
                SessionResourceErrorKind::Unavailable {
                    detail: "injected lost COMMIT acknowledgment".to_owned(),
                },
            )),
            _ => result,
        }
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, peri_acp_types::store::MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.inner.apply_message_projections(id, updates).await
    }

    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()> {
        self.inner.rewind_history(id, boundary).await
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.inner.remove_history_entries(id, ids).await
    }

    async fn update_session_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.inner.update_session_meta(id, patch).await
    }

    async fn delete_session_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.delete_session_tree(id).await
    }

    async fn recover_session_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        self.inner.recover_session_persistence(id).await
    }

    async fn drain_persistence(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.drain_persistence(id).await
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
    _repo: tempfile::TempDir,
    /// 持有执行所有权：门面上的写入要求本 root 有活 owner。
    _lease: Arc<dyn SessionExecutionLease>,
    store: Arc<ControlledStore>,
    thread_id: ThreadId,
    history: Vec<BaseMessage>,
    report_id: MessageId,
    result: peri_acp_types::session::PromptResult,
    done_count: usize,
    done_reasons: Vec<String>,
}
async fn run_case(mode: CommitMode, pause: HandlerPause, pre_cancel: bool) -> Case {
    let dir = tempfile::tempdir().unwrap();
    let repo = git_repository();
    let cancel = AgentCancellationToken::new();
    let inner: Arc<dyn SessionResources> = Arc::new(
        SessionResourcesImpl::open(dir.path().join("manual.db"))
            .await
            .unwrap(),
    );
    let store = Arc::new(ControlledStore {
        inner: Arc::clone(&inner),
        mode,
        cancel: cancel.clone(),
        fail_reload: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    // 真门面建会话：绑定 + frozen + 执行代际一次落盘，写入门禁才有 owner。
    let workspace = inner.resolve_workspace(repo.path()).await.unwrap();
    let thread_id = uuid::Uuid::now_v7().to_string();
    let session = NewSession {
        thread_id: thread_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: Some("manual compact".to_owned()),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: workspace.project_id,
            workspace_id: workspace.execution_registration_id,
            cwd_relative_to_workspace: workspace.relative_cwd.clone(),
        },
        frozen: FrozenSnapshotBytes::new("{\"version\":1,\"manual\":true}"),
    };
    let lease = inner.create_session(&session).await.unwrap();
    let history = vec![
        BaseMessage::human("old manual question"),
        BaseMessage::ai("old manual answer"),
    ];
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let report_id = MessageId::new();
    let mut payloads: Vec<_> = history
        .iter()
        .cloned()
        .map(PersistedPayload::Message)
        .collect();
    payloads.push(PersistedPayload::SystemReminder {
        id: report_id,
        reminder: TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Task,
                source: ReminderSource("subagent".into()),
                kind: "completed".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: "MANUAL_REPORT_MUST_SURVIVE_CANCEL".to_owned(),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap(),
    });
    inner.append_history(&thread_id, &payloads).await.unwrap();
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
    req.history_payloads = payloads;
    req.cwd = workspace.cwd.to_str().unwrap();
    req.session_resources = Some(store.clone());
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
    let done_reasons = sink.push_done_stop_reasons.lock().unwrap().clone();
    Case {
        _dir: dir,
        _repo: repo,
        _lease: lease,
        store,
        thread_id,
        history,
        report_id,
        result,
        done_count: sink.push_done_count(),
        done_reasons,
    }
}

/// 磁盘事实（payload/flags 同一次一致快照读取）。
///
/// 直读**未被注入**的真实门面：包装层注入的是「调用方看到的读失败」，验证落库事实时
/// 不能连它一起读，否则断言会被注入本身带偏。
async fn stored_snapshot(case: &Case) -> SessionSnapshot {
    case.store
        .inner
        .load_session_snapshot(&case.thread_id)
        .await
        .unwrap()
}

async fn assert_durable_summary(case: &Case) {
    let payloads = stored_snapshot(case).await.payloads;
    assert!(payloads.iter().any(|payload| payload
        .as_message()
        .is_some_and(|message| message.content().contains("manual committed summary"))));
    assert!(payloads
        .iter()
        .any(|payload| payload.id() == case.report_id));
    let flags = stored_snapshot(case).await.flags;
    assert!(
        flags[&case.report_id].excluded,
        "已确认或磁盘已提交的 Full 必须一并排除旧报告"
    );
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
        stored_snapshot(&case)
            .await
            .payloads
            .iter()
            .filter(|payload| payload.as_message().is_some())
            .count(),
        2
    );
    assert!(stored_snapshot(&case).await.flags.is_empty());
    assert!(stored_snapshot(&case)
        .await
        .payloads
        .iter()
        .any(|p| p.id() == case.report_id));
    assert_eq!(case.done_count, 1);
}

#[tokio::test]
async fn test_manual_compact_cancel_after_sql_commit_requires_reload_and_keeps_summary() {
    let case = run_case(CommitMode::CancelAfter, HandlerPause::None, false).await;
    assert!(case.result.persistence_inconsistent);
    assert!(!case.result.ok);
    assert!(case.result.failure.is_some());
    assert_eq!(case.result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(
        case.done_reasons,
        ["end_turn"],
        "未确认提交必须保留 Internal/reload 终态"
    );
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
    assert_eq!(case.done_reasons, ["cancelled"]);
    assert_eq!(case.result.messages.len(), 1);
    assert!(case.result.messages[0]
        .content()
        .contains("manual committed summary"));
    let stored = stored_snapshot(&case).await.payloads;
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
    assert_eq!(case.done_reasons, ["cancelled"]);
    assert_eq!(case.store.calls.load(Ordering::SeqCst), 0);
    assert!(stored_snapshot(&case).await.flags.is_empty());
    assert!(case
        .result
        .persisted_payloads
        .iter()
        .any(|p| p.id() == case.report_id));
    assert_eq!(case.result.messages.len(), 2);
    assert_eq!(case.done_count, 1);
}
