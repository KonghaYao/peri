use super::*;
use crate::session::test_resources::mock::MockSessionResources;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::*;
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::tasks::TaskManager as _;
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// 可控 gate：让 Working 写停在「已进入、尚未提交」，或按需失败；
/// `pause_after_commit` 额外停在「已提交、调用尚未返回」的取消窗口。
pub(super) struct WorkStateGate {
    pub(super) entered: tokio::sync::Notify,
    pub(super) release: tokio::sync::Semaphore,
    pub(super) fail: bool,
    pub(super) writes: AtomicUsize,
    pub(super) pause_after_commit: std::sync::atomic::AtomicBool,
    pub(super) committed: tokio::sync::Notify,
    pub(super) post_release: tokio::sync::Semaphore,
}

/// 延迟旧认领的结清：让「旧补偿到达」停在新 run 已经进入 `Active + Working` 之后。
///
/// 新实现把状态与工作意图的恢复合并到一次结算里，原先「状态已回滚、工作意图回滚
/// 被延迟」的窗口不再存在；把结清整体卡在入口，复现同一个归属歧义：迟到的旧补偿
/// 看到的新 run 状态与自己的中间态完全相同。
pub(super) struct WorkRollbackGate {
    pub(super) entered: tokio::sync::Notify,
    pub(super) release: tokio::sync::Semaphore,
    pub(super) committed: tokio::sync::Notify,
}

impl Default for WorkRollbackGate {
    fn default() -> Self {
        Self {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            committed: tokio::sync::Notify::new(),
        }
    }
}

/// 测试观察：保留认领 handle，允许用例在新一轮运行开始后重放迟到的补偿。
#[derive(Clone)]
pub(super) struct ReplayClaim(Arc<ReplayClaimInner>);

struct ReplayClaimInner {
    inner: Box<dyn ChildResumeClaim>,
    rollback_gate: Option<Arc<WorkRollbackGate>>,
}

#[async_trait::async_trait]
impl ChildResumeClaim for ReplayClaim {
    async fn mark_running(&self) -> SessionResourceResult<()> {
        self.0.inner.mark_running().await
    }
    async fn hand_off_to_background(&self) -> SessionResourceResult<()> {
        self.0.inner.hand_off_to_background().await
    }
    async fn mark_failed(&self) -> SessionResourceResult<()> {
        if let Some(gate) = &self.0.rollback_gate {
            gate.entered.notify_one();
            gate.release.acquire().await.unwrap().forget();
            gate.committed.notify_one();
        }
        self.0.inner.mark_failed().await
    }
    async fn mark_terminated(&self) -> SessionResourceResult<()> {
        self.0.inner.mark_terminated().await
    }
    async fn finish_with_status(&self, status: AgentStatus) -> SessionResourceResult<bool> {
        self.0.inner.finish_with_status(status).await
    }
}

struct ClaimResources {
    inner: Arc<MockSessionResources>,
    read_error: Mutex<Option<SessionResourceError>>,
    claim_error: Mutex<Option<SessionResourceError>>,
    claims: AtomicUsize,
    commit_before_error: bool,
    handoff_gate: Option<Arc<HandoffGate>>,
    work_state_gate: Option<Arc<WorkStateGate>>,
    rollback_gate: Option<Arc<WorkRollbackGate>>,
    replays: Mutex<Vec<ReplayClaim>>,
    terminal_error: bool,
}

impl ClaimResources {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MockSessionResources::new(),
            read_error: Mutex::new(None),
            claim_error: Mutex::new(None),
            claims: AtomicUsize::new(0),
            commit_before_error: false,
            handoff_gate: None,
            work_state_gate: None,
            rollback_gate: None,
            replays: Mutex::new(Vec::new()),
            terminal_error: false,
        })
    }

    /// 最近一次认领的 handle（测试用它重放迟到的补偿）。
    pub(super) fn last_claim(&self) -> ReplayClaim {
        self.replays
            .lock()
            .unwrap()
            .last()
            .expect("no resume claim recorded")
            .clone()
    }
}

#[async_trait::async_trait]
impl SessionResources for ClaimResources {
    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        if let Some(error) = self.read_error.lock().unwrap().take() {
            return Err(error);
        }
        self.inner.load_session_meta(id).await
    }

    async fn claim_child_resume(
        &self,
        child: &ThreadId,
        root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        self.claims.fetch_add(1, Ordering::SeqCst);
        if self.commit_before_error {
            let _handle = self.inner.claim_child_resume(child, root).await?;
        }
        if let Some(error) = self.claim_error.lock().unwrap().take() {
            return Err(error);
        }
        let inner = self.inner.claim_child_resume(child, root).await?;
        let inner: Box<dyn ChildResumeClaim> = match &self.handoff_gate {
            Some(gate) => Box::new(GatedClaim {
                inner,
                gate: gate.clone(),
                handed_off: std::sync::atomic::AtomicBool::new(false),
            }),
            None => inner,
        };
        let replay = ReplayClaim(Arc::new(ReplayClaimInner {
            inner,
            rollback_gate: self.rollback_gate.clone(),
        }));
        self.replays.lock().unwrap().push(replay.clone());
        Ok(Box::new(replay))
    }
    async fn inspect_availability(
        &self,
        session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        self.inner.inspect_availability(session).await
    }
    async fn resolve_workspace(&self, cwd: &Path) -> SessionResourceResult<ResolvedWorkspace> {
        self.inner.resolve_workspace(cwd).await
    }
    async fn validate_session(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.inner.validate_session(id, workspace).await
    }
    async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.finish_close(id).await
    }
    async fn close_settlement(&self, id: &ThreadId) -> SessionResourceResult<CloseSettlement> {
        self.inner.close_settlement(id).await
    }
    async fn create_session(&self, input: &NewSession) -> SessionResourceResult<()> {
        self.inner.create_session(input).await
    }
    async fn abandon_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.abandon_initialization(id).await
    }
    async fn begin_initialization(
        &self,
        draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>> {
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
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.inner
            .append_reminder_if_absent(id, message_id, reminder)
            .await
    }
    async fn mq_enqueue(
        &self,
        thread_id: &ThreadId,
        input: peri_acp_types::session::MqInput,
    ) -> SessionResourceResult<peri_acp_types::session::MqEnqueueOutcome> {
        self.inner.mq_enqueue(thread_id, input).await
    }
    async fn mq_query(
        &self,
        thread_id: &ThreadId,
        filter: peri_acp_types::session::MqFilter,
    ) -> SessionResourceResult<Vec<peri_acp_types::session::MqEntry>> {
        self.inner.mq_query(thread_id, filter).await
    }
    async fn mq_cancel(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<peri_acp_types::session::MqCancelOutcome> {
        self.inner.mq_cancel(thread_id, id).await
    }
    async fn mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: peri_acp_types::session::MqReceiveSelection,
    ) -> SessionResourceResult<peri_acp_types::session::MqReceiveOutcome> {
        self.inner.mq_receive(thread_id, selection).await
    }
    async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.mq_clear(thread_id).await
    }
    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.mark_session_closing(id).await
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
    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.inner.apply_compaction(id, change).await
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
        clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        self.inner
            .rewind_history(id, boundary, clear_excluded)
            .await
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
        if self.terminal_error && patch.status.is_some_and(|status| !status.is_active()) {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Unavailable {
                    detail: "terminal store offline".into(),
                },
            ));
        }
        self.inner.update_session_meta(id, patch).await
    }
    async fn settle_terminal_without_result(
        &self,
        child: &ThreadId,
        status: AgentStatus,
        work_state: peri_acp_types::thread::WorkState,
    ) -> SessionResourceResult<peri_acp_types::session_resources::TerminalSettlementOutcome> {
        if self.terminal_error {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Unavailable {
                    detail: "terminal store offline".into(),
                },
            ));
        }
        self.inner
            .settle_terminal_without_result(child, status, work_state)
            .await
    }
    async fn set_work_state(
        &self,
        id: &ThreadId,
        state: peri_acp_types::thread::WorkState,
    ) -> SessionResourceResult<()> {
        use peri_acp_types::thread::WorkState;
        let gated = state == WorkState::Working;
        if gated {
            if let Some(gate) = &self.work_state_gate {
                gate.writes.fetch_add(1, Ordering::SeqCst);
                gate.entered.notify_one();
                gate.release.acquire().await.unwrap().forget();
                if gate.fail {
                    return Err(SessionResourceError::new(
                        SessionResourceErrorKind::Unavailable {
                            detail: "work state offline".into(),
                        },
                    ));
                }
            }
        }
        let written = self.inner.set_work_state(id, state).await;
        // 「已提交、调用尚未返回」：调用方可以被取消，但写入已经落地。
        if gated {
            if let Some(gate) = &self.work_state_gate {
                if gate.pause_after_commit.load(Ordering::SeqCst) {
                    gate.committed.notify_one();
                    gate.post_release.acquire().await.unwrap().forget();
                }
            }
        }
        written
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

#[path = "background_handoff_test.rs"]
mod background_handoff_tests;
use background_handoff_tests::{GatedClaim, HandoffGate};

async fn acquire_error(
    store: Arc<dyn SessionResources>,
    thread_id: &str,
) -> Box<dyn std::error::Error + Send + Sync> {
    match ResumeClaim::acquire(store, thread_id.into(), thread_id.into(), None).await {
        Err(error) => error,
        Ok(_) => panic!("claim unexpectedly succeeded"),
    }
}

#[tokio::test]
async fn cancel_running_claim_finalizes_only_after_resource_close_is_confirmed() {
    use crate::session::subagent::{ParkedChildOwner, SubagentHost};
    use crate::session::{FrozenContext, MessageTranscript, Session};

    for persistence_failure in [true, false] {
        let store = MockSessionResources::new();
        store
            .update_thread_status(&CHILD.into(), "done")
            .await
            .unwrap();
        let (_, mut claim) = ResumeClaim::acquire(store.clone(), CHILD.into(), CHILD.into(), None)
            .await
            .unwrap();
        assert_eq!(
            store
                .load_session_meta(&CHILD.into())
                .await
                .unwrap()
                .agent_status,
            AgentStatus::Active
        );
        let session = Session::new(
            Arc::from("/tmp"),
            FrozenContext::builder().build(),
            Some(CHILD.into()),
        );
        let history = MockSessionResources::new();
        *session.transcript().write() =
            MessageTranscript::new().with_persistence(history.clone(), CHILD.into());
        let manager = Arc::new(crate::agent::async_tasks::TaskManager::new());
        let owner = ParkedChildOwner::new(
            CHILD.into(),
            CHILD.into(),
            manager.clone(),
            session.queue().clone(),
            Some(history.clone()),
            None,
        );
        owner.set_writer(session.transcript().read().persist_tx_handle());
        session.set_subagent_host(SubagentHost {
            task_manager: Some(manager),
            child_owner: Some(owner.clone()),
            ..Default::default()
        });
        if persistence_failure {
            // 只令 transcript 写失败；meta 仍可写，避免把 Active 误归因于状态写失败。
            history.restrict_to_history_read_only();
            session
                .transcript()
                .write()
                .append(crate::messages::BaseMessage::human("未确认历史"));
        } else {
            let permit = owner
                .acquire(session.start_turn().execution_binding())
                .unwrap();
            permit.finish(Err("执行结算未确认".into()));
        }
        claim.mark_running(session.clone());
        claim.decide(ClaimDecision::CancelRunning(session.clone()));
        let error = (&mut claim.worker).await.unwrap().unwrap_err();
        assert!(session.subagent_host().unwrap().close_state.is_closing());
        assert!(owner.failure().is_some());
        let expected = if persistence_failure {
            assert!(
                error.starts_with("Incomplete: child history persistence unconfirmed"),
                "{error}"
            );
            AgentStatus::Active
        } else {
            assert!(
                error.starts_with("Incomplete: child settlement is unconfirmed"),
                "{error}"
            );
            AgentStatus::Cancelled
        };
        assert_eq!(
            store
                .load_session_meta(&CHILD.into())
                .await
                .unwrap()
                .agent_status,
            expected
        );
    }
}

const CHILD: &str = "00000000-0000-0000-0000-000000000000";

#[tokio::test]
async fn missing_thread_survives_worker_channel_as_invalid_input() {
    let store = ClaimResources::new();
    let error = acquire_error(store.clone(), CHILD).await;
    let typed = error.downcast_ref::<EffectiveToolError>().unwrap();
    assert_eq!(typed.code, EffectiveToolErrorCode::InvalidInput);
    assert_eq!(
        typed.message,
        format!("resume_subagent: thread not found: {CHILD}")
    );
    assert_eq!(store.claims.load(Ordering::SeqCst), 0);
    assert!(store.inner.statuses().is_empty());
}

#[tokio::test]
async fn invalid_identity_survives_worker_channel_without_claiming() {
    let store = ClaimResources::new();
    let error = acquire_error(store.clone(), "invalid-id").await;
    assert_eq!(
        error.downcast_ref::<EffectiveToolError>().unwrap().code,
        EffectiveToolErrorCode::InvalidInput
    );
    assert_eq!(store.claims.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn active_thread_is_typed_application_failure_without_claiming() {
    let store = ClaimResources::new();
    store
        .inner
        .update_thread_status(&CHILD.into(), "active")
        .await
        .unwrap();
    let before = store.inner.statuses();
    let error = acquire_error(store.clone(), CHILD).await;
    let typed = error.downcast_ref::<EffectiveToolError>().unwrap();
    assert_eq!(typed.code, EffectiveToolErrorCode::ApplicationFailed);
    assert!(typed.message.contains("still active"));
    assert_eq!(store.claims.load(Ordering::SeqCst), 0);
    assert_eq!(store.inner.statuses(), before);
}

#[tokio::test]
async fn metadata_read_errors_keep_resource_kind_and_effect() {
    for kind in [
        SessionResourceErrorKind::ReadOnlyStore,
        SessionResourceErrorKind::Corrupt {
            detail: "unreadable metadata".into(),
        },
        SessionResourceErrorKind::Unavailable {
            detail: "offline".into(),
        },
        SessionResourceErrorKind::Timeout,
        SessionResourceErrorKind::PersistenceUncertain {
            thread_id: Some(CHILD.into()),
        },
    ] {
        let store = ClaimResources::new();
        let expected = SessionResourceError::new(kind);
        let expected_kind = format!("{:?}", expected.kind());
        let expected_effect = expected.effect();
        *store.read_error.lock().unwrap() = Some(expected);
        let error = acquire_error(store.clone(), CHILD).await;
        assert!(error.downcast_ref::<EffectiveToolError>().is_none());
        let typed = error.downcast_ref::<SessionResourceError>().unwrap();
        assert_eq!(format!("{:?}", typed.kind()), expected_kind);
        assert_eq!(typed.effect(), expected_effect);
        assert!(!error.to_string().contains("thread not found"));
        assert_eq!(store.claims.load(Ordering::SeqCst), 0);
        assert!(store.inner.statuses().is_empty());
    }
}

#[tokio::test]
async fn claim_rejections_keep_resource_kind_without_application_failure() {
    for kind in [
        SessionResourceErrorKind::ReadOnlyStore,
        SessionResourceErrorKind::NotFound,
        SessionResourceErrorKind::InvalidInput {
            detail: "child session is still active".into(),
        },
    ] {
        let store = ClaimResources::new();
        store
            .inner
            .update_thread_status(&CHILD.into(), "done")
            .await
            .unwrap();
        let before = store.inner.statuses();
        let expected = SessionResourceError::new(kind);
        let expected_kind = format!("{:?}", expected.kind());
        *store.claim_error.lock().unwrap() = Some(expected);
        let manager = crate::agent::async_tasks::TaskManager::new();
        let guard =
            peri_acp_types::tasks::TaskManager::begin_external_execution(&manager, "resume")
                .unwrap();
        let error = ResumeClaim::acquire(store.clone(), CHILD.into(), CHILD.into(), Some(guard))
            .await
            .err()
            .expect("claim must reject");
        assert!(error.downcast_ref::<EffectiveToolError>().is_none());
        let typed = error.downcast_ref::<SessionResourceError>().unwrap();
        assert_eq!(format!("{:?}", typed.kind()), expected_kind);
        assert_eq!(typed.effect(), MutationOutcome::NotApplied);
        assert_eq!(store.claims.load(Ordering::SeqCst), 1);
        assert_eq!(store.inner.statuses(), before);
        assert!(manager.is_execution_idle());
    }
}

#[tokio::test]
async fn committed_claim_with_unknown_ack_stays_unknown_and_active() {
    let mut store = ClaimResources::new();
    Arc::get_mut(&mut store).unwrap().commit_before_error = true;
    store
        .inner
        .update_thread_status(&CHILD.into(), "done")
        .await
        .unwrap();
    *store.claim_error.lock().unwrap() = Some(SessionResourceError::persistence_uncertain(Some(
        CHILD.into(),
    )));
    let manager = crate::agent::async_tasks::TaskManager::new();
    let guard =
        peri_acp_types::tasks::TaskManager::begin_external_execution(&manager, "resume").unwrap();
    let error = ResumeClaim::acquire(store.clone(), CHILD.into(), CHILD.into(), Some(guard))
        .await
        .err()
        .expect("claim ACK must remain unknown");
    assert!(error.downcast_ref::<EffectiveToolError>().is_none());
    let typed = error.downcast_ref::<SessionResourceError>().unwrap();
    assert!(
        matches!(typed.kind(), SessionResourceErrorKind::PersistenceUncertain { thread_id: Some(thread_id) } if thread_id == CHILD)
    );
    assert_eq!(typed.effect(), MutationOutcome::Unknown);
    assert_eq!(store.claims.load(Ordering::SeqCst), 1);
    assert!(store
        .inner
        .load_session_meta(&CHILD.into())
        .await
        .unwrap()
        .agent_status
        .is_active());
    assert!(!manager.is_execution_idle());
}

async fn resumable_child(store: &ClaimResources) {
    let mut meta = ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = CHILD.into();
    meta.agent_status = AgentStatus::Done;
    meta.work_state = peri_acp_types::thread::WorkState::Stopped;
    store.inner.create_resumable_thread(meta).await.unwrap();
}

/// 正常 Finish：终态由认领在归属校验内提交，并恢复认领前的工作意图。
#[tokio::test]
async fn finish_writes_terminal_status_for_owned_claim() {
    use peri_acp_types::thread::WorkState;
    let store = ClaimResources::new();
    resumable_child(&store).await;
    let (_, claim) = ResumeClaim::acquire(store.clone(), CHILD.into(), CHILD.into(), None)
        .await
        .unwrap();
    store
        .inner
        .set_work_state(&CHILD.into(), WorkState::Working)
        .await
        .unwrap();

    claim.finish(AgentStatus::Error).await.unwrap();
    let meta = store.inner.load_session_meta(&CHILD.into()).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Stopped);
}

/// [回归] 旧认领结清后新 run 认领成功；迟到的 Finish 终态不得覆盖新的 Active。
///
/// 复现原缺陷窗口：旧认领的结清已提交、终态尚未写入时，新 run 完成认领；
/// 旧认领随后提交终态会把新认领的 `Active` 覆盖掉。
#[tokio::test]
async fn stale_claim_finish_does_not_override_new_claim() {
    let store = ClaimResources::new();
    resumable_child(&store).await;
    let (_, old) = ResumeClaim::acquire(store.clone(), CHILD.into(), CHILD.into(), None)
        .await
        .unwrap();
    store.last_claim().mark_terminated().await.unwrap();
    let new_claim = store
        .claim_child_resume(&CHILD.into(), &CHILD.into())
        .await
        .unwrap();
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Active
    );

    old.finish(AgentStatus::Cancelled).await.unwrap();

    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Active,
        "旧认领的迟到终态不得覆盖新认领"
    );
    drop(new_claim);
}
