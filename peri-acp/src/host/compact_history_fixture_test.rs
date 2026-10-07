//! Ordinary Compact history and cancellation regressions through the production host.

use super::*;
use crate::host::{prompt::finish_prompt_turn, SessionState, SharedSessions};
use peri_acp_types::{
    messages::MessageId,
    session_resources::{
        ChildResumeClaim, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes, NewSession,
        NewSessionMeta, PersistenceRecovery, RewindBoundary, SessionAvailability, SessionMetaPatch,
        SessionResourceResult, SessionResources, SessionSnapshot,
    },
    store::{CompactionChange, MessageFlags, PersistedPayload},
    thread::{ThreadId, ThreadMeta},
    workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding},
};
use std::collections::HashMap;

#[path = "compact_command_test.rs"]
mod compact_command_tests;

const SUMMARY: &str = "COMMITTED_COMPACT_HISTORY_SUMMARY";
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

struct CompactStore {
    inner: Arc<dyn SessionResources>,
    compact_commits: AtomicUsize,
}

#[async_trait]
impl SessionResources for CompactStore {
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

    async fn create_session(&self, input: &NewSession) -> SessionResourceResult<()> {
        self.inner.create_session(input).await
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

    async fn abandon_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.abandon_initialization(id).await
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
        self.inner.load_session_snapshot(id).await
    }

    async fn load_session_binding(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::BindingState> {
        self.inner.load_session_binding(id).await
    }

    async fn validate_bound_workspace(
        &self,
        id: &ThreadId,
        check: peri_acp_types::session_resources::BindingRecheck,
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

    async fn finish_close(&self, token: &ThreadId) -> SessionResourceResult<()> {
        self.inner.finish_close(token).await
    }

    async fn close_settlement(
        &self,
        token: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::CloseSettlement> {
        self.inner.close_settlement(token).await
    }

    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.inner.is_session_closing(id).await
    }

    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        self.inner.save_fork(fork).await
    }

    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        self.inner.save_child(child).await
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
        self.inner.apply_compaction(id, change).await?;
        if !change.appended_messages.is_empty() {
            self.compact_commits.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
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

impl CompactStore {
    /// 夹具侧直接读回持久化历史（走门面的一致快照，不另开连接）。
    async fn load_payloads(&self, id: &ThreadId) -> anyhow::Result<Vec<PersistedPayload>> {
        Ok(self.inner.load_session_snapshot(id).await?.payloads)
    }
}

fn make_host_sessions(ctx: &SessionContext, payloads: Vec<PersistedPayload>) -> SharedSessions {
    let state = SessionState {
        session_id: ctx.session_id.clone(),
        thread_id: ctx.thread_id.clone().unwrap(),
        cwd: ctx.cwd.clone(),
        environment: None,
        closing: false,
        history: payloads
            .iter()
            .filter_map(|payload| payload.as_message().cloned())
            .collect(),
        history_payloads: payloads,
        cancel_token: Some(ctx.cancel.clone()),
        continuation_armed: false,
        continuation_epoch: 0,
        continuation_in_flight: false,
        continuation_mq_steering_pending: false,
        frozen: Some(make_sentinel_frozen()),
        recall_items: vec![],
        agent_pool: AgentPool::new(),
        workflow_middleware: None,
        title: None,
        tags: vec![],
    };
    Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        ctx.session_id.clone(),
        state,
    )])))
}

async fn make_compact_context(
    dir: &tempfile::TempDir,
    model: Arc<dyn Model>,
) -> (SessionContext, Arc<CompactStore>, SharedSessions) {
    // 夹具与生产同构：门面来自同一次打开，并经公共会话创建入口保存完整身份。
    let (_bridge, facade) = peri_resources::sessions::open_store_and_facade_for_tests(
        dir.path().join("compact-history.db"),
    )
    .await
    .unwrap();
    let store = Arc::new(CompactStore {
        inner: Arc::new(facade),
        compact_commits: AtomicUsize::new(0),
    });
    let cwd = dir.path().to_str().unwrap();
    let meta = ThreadMeta::new_at(cwd, peri_time::now_wall());
    let thread_id = meta.id.clone();
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    let cwd = workspace.cwd.to_str().unwrap();
    let frozen = make_sentinel_frozen();
    store
        .create_session(&NewSession {
            thread_id: thread_id.clone(),
            created_at: meta.created_at.to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: cwd.into(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(
                crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap(),
            ),
        })
        .await
        .unwrap();
    let mut ctx = make_session_context(&thread_id).await;
    ctx.cwd = cwd.into();
    ctx.thread_id = Some(thread_id.clone());
    ctx.session_resources = Some(store.clone());
    ctx.session_access = None;
    let history = vec![BaseMessage::human(OLD), BaseMessage::ai("old answer")];
    store
        .append_history(
            &thread_id,
            &history
                .iter()
                .cloned()
                .map(PersistedPayload::Message)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    ctx.primary_llm_factory = Some(Arc::new(move || model.clone()));
    execution_fixture::initialize_runtime(&mut ctx, None);
    let sessions = make_host_sessions(
        &ctx,
        history.into_iter().map(PersistedPayload::Message).collect(),
    );
    (ctx, store, sessions)
}

async fn make_compact_turn(
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
        MessageContent::text("continue ordinary session"),
        false,
        state.history.clone(),
        stage_build,
    );
    turn.history_payloads = state.history_payloads.clone();
    turn.frozen = state.frozen.clone();
    turn
}

async fn assert_next_turn_sees_summary(mut ctx: SessionContext, sessions: &SharedSessions) {
    ctx.session_access = None;
    execution_fixture::initialize_runtime(&mut ctx, None);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: requests.clone(),
    });
    ctx.primary_llm_factory = Some(Arc::new(move || model.clone()));
    ctx.cancel = AgentCancellationToken::new();
    let turn = make_compact_turn(&ctx, sessions, false).await;
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
