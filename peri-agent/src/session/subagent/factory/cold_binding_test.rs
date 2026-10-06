use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{work::*, *};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Clone, Copy)]
enum AckMode {
    Normal,
    Applied,
    Unknown,
    NotApplied,
    ConflictingReceipt,
    ConflictingResolvedReceipt,
    ConcurrentWriter,
}

struct BindingResources {
    inner: Arc<dyn SessionResources>,
    mode: AckMode,
    reads: AtomicUsize,
    writes: AtomicUsize,
    resolutions: AtomicUsize,
    resolution_pause: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    unknown_command: parking_lot::Mutex<Option<WorkCommand>>,
}

impl BindingResources {
    fn new(inner: Arc<dyn SessionResources>, mode: AckMode) -> Self {
        Self {
            inner,
            mode,
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            resolutions: AtomicUsize::new(0),
            resolution_pause: None,
            unknown_command: parking_lot::Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl SessionResources for BindingResources {
    async fn inspect_availability(
        &self,
        session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        self.inner.inspect_availability(session).await
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

    async fn inspect_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkInspection> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let snapshot = self.inner.inspect_work(query).await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(snapshot)
    }

    async fn apply_work_mutation(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if matches!(
            self.mode,
            AckMode::Applied
                | AckMode::Unknown
                | AckMode::NotApplied
                | AckMode::ConflictingResolvedReceipt
        ) {
            *self.unknown_command.lock() = Some(command.clone());
        }
        if matches!(self.mode, AckMode::ConcurrentWriter) {
            let snapshot = self
                .inner
                .inspect_work(&WorkQuery::new(
                    command.session_id.clone(),
                    WorkSelector::Head,
                ))
                .await?;
            let mut external = WorkCommand {
                session_id: command.session_id.clone(),
                recipient_lifecycle: command.recipient_lifecycle,
                mutation_id: "external-writer".into(),
                action: WorkAction::BindResourceOwners {
                    expected_revision: snapshot.head.change_seq,
                    connections_json: "[]".into(),
                    authorization_ref: "child-authorization".into(),
                },
            };
            external.mutation_id = format!("external-writer:{}", external.digest()?);
            assert_eq!(
                self.inner.apply_work_mutation(&external).await?.decision,
                WorkDecision::Accepted
            );
        }
        if matches!(self.mode, AckMode::Unknown | AckMode::NotApplied) {
            return Err(SessionResourceError::persistence_uncertain(Some(
                command.session_id.clone(),
            )));
        }
        let mut receipt = self.inner.apply_work_mutation(command).await?;
        match self.mode {
            AckMode::Applied | AckMode::ConflictingResolvedReceipt => Err(
                SessionResourceError::persistence_uncertain(Some(command.session_id.clone())),
            ),
            AckMode::ConflictingReceipt => {
                receipt.mutation_id = "wrong-mutation".into();
                Ok(receipt)
            }
            _ => Ok(receipt),
        }
    }

    async fn resolve_work_mutation(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        assert_eq!(self.unknown_command.lock().as_ref(), Some(command));
        if let Some((entered, release)) = &self.resolution_pause {
            entered.notify_one();
            release.notified().await;
        }
        match self.mode {
            AckMode::Unknown => Ok(WorkResolution::Unknown),
            AckMode::NotApplied => Ok(WorkResolution::NotApplied),
            AckMode::ConflictingResolvedReceipt => {
                let resolution = self.inner.resolve_work_mutation(command).await?;
                let WorkResolution::Applied { mut receipt } = resolution else {
                    panic!("test mutation must be applied");
                };
                receipt.session_id = "wrong-session".into();
                Ok(WorkResolution::Applied { receipt })
            }
            _ => self.inner.resolve_work_mutation(command).await,
        }
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

    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.inner.load_session_meta(id).await
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

async fn binding_effect(
    resources: &dyn SessionResources,
    session_id: &str,
    invocation_id: &str,
) -> Effect {
    crate::session::work_access::effect(resources, session_id, invocation_id)
        .await
        .unwrap()
}

async fn prepare_invocations(session: &TestSession, count: usize) -> Vec<Effect> {
    let resources = session.resources();
    let session_id = session.thread_id();
    for index in 0..count {
        let snapshot = resources
            .inspect_work(&WorkQuery::new(session_id.clone(), WorkSelector::Head))
            .await
            .unwrap();
        let arguments = "{}".to_owned();
        let arguments_ref = crate::agent::stages::prepare_work_evidence(
            resources.as_ref(),
            &session_id,
            arguments.as_bytes().to_vec(),
        )
        .await
        .unwrap();
        let digest = format!("{:x}", Sha256::digest(arguments.as_bytes()));
        let receipt = resources
            .apply_work_mutation(&WorkCommand {
                session_id: session_id.clone(),
                recipient_lifecycle: 1,
                mutation_id: format!("prepare-{index}"),
                action: WorkAction::PrepareInvocation {
                    expected_revision: snapshot.head.change_seq,
                    intent: InvocationIntent {
                        invocation_id: format!("invocation-{index}"),
                        tool_call_id: format!("call-{index}"),
                        tool_name: "subagent".into(),
                        arguments: arguments_ref.clone(),
                        arguments_digest: digest.clone(),
                        effective_tool_name: "subagent".into(),
                        effective_arguments: arguments_ref,
                        effective_arguments_digest: digest,
                        owner_identity: "child-owner".into(),
                        scope_id: session_id.clone(),
                        scope_epoch: Some(1),
                        authorization_ref: "child-authorization".into(),
                        recovery_locator: format!("child-{index}"),
                    },
                },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
    }
    let mut effects = Vec::new();
    for index in 0..count {
        effects.push(
            binding_effect(
                resources.as_ref(),
                &session_id,
                &format!("invocation-{index}"),
            )
            .await,
        );
    }
    effects
}

#[tokio::test]
async fn five_parallel_delegation_bindings_all_persist_without_revision_collisions() {
    let session = TestSession::open().await;
    let invocations = prepare_invocations(&session, 5).await;
    let first = Arc::new(BindingResources::new(session.resources(), AckMode::Normal));
    let second = Arc::new(BindingResources::new(session.resources(), AckMode::Normal));
    let start = Arc::new(tokio::sync::Barrier::new(5));
    let mut tasks = tokio::task::JoinSet::new();
    for (index, invocation) in invocations.iter().cloned().enumerate() {
        let resources = if index % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        let session_id = session.thread_id();
        let start = start.clone();
        tasks.spawn(async move {
            start.wait().await;
            let receipt = bind_delegation_task(
                resources.as_ref(),
                &session_id,
                &invocation,
                &format!("task-{index}"),
            )
            .await
            .unwrap();
            assert_eq!(receipt.decision, WorkDecision::Accepted);
            receipt.mutation_id
        });
    }
    let mutations = tokio::time::timeout(Duration::from_secs(5), async {
        let mut mutations = BTreeSet::new();
        while let Some(result) = tasks.join_next().await {
            assert!(mutations.insert(result.unwrap()));
        }
        mutations
    })
    .await
    .unwrap();
    assert_eq!(mutations.len(), 5);
    assert_eq!(
        first.writes.load(Ordering::SeqCst) + second.writes.load(Ordering::SeqCst),
        5
    );
    for (index, invocation) in invocations.iter().enumerate() {
        let effect = binding_effect(
            session.resources().as_ref(),
            &session.thread_id(),
            &invocation.intent.invocation_id,
        )
        .await;
        let binding = effect.binding.as_ref().unwrap();
        assert_eq!(binding.owner_task_id, format!("task-{index}"));
        assert_eq!(binding.initiator_session_id, session.thread_id());
        assert_eq!(binding.owner_identity, invocation.intent.owner_identity);
        assert_eq!(
            binding.authorization_ref,
            invocation.intent.authorization_ref
        );
        assert_eq!(binding.recovery_locator, invocation.intent.recovery_locator);
        assert_eq!(binding.recipient_lifecycle, invocation.recipient_lifecycle);
    }
}

#[tokio::test]
async fn unknown_binding_ack_remains_incomplete_without_retry_or_downgrade() {
    for mode in [AckMode::Unknown, AckMode::NotApplied] {
        let session = TestSession::open().await;
        let invocation = prepare_invocations(&session, 1).await.remove(0);
        let resources = BindingResources::new(session.resources(), mode);
        let error = bind_delegation_task(&resources, &session.thread_id(), &invocation, "task")
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Incomplete: delegation task binding ACK unknown"
        );
        assert_eq!(resources.writes.load(Ordering::SeqCst), 1);
        assert_eq!(resources.resolutions.load(Ordering::SeqCst), 1);
        assert!(binding_effect(
            session.resources().as_ref(),
            &session.thread_id(),
            &invocation.intent.invocation_id
        )
        .await
        .binding
        .is_none());
    }
}

#[tokio::test]
async fn lost_binding_ack_succeeds_only_with_matching_applied_receipt() {
    let session = TestSession::open().await;
    let invocation = prepare_invocations(&session, 1).await.remove(0);
    let resources = BindingResources::new(session.resources(), AckMode::Applied);
    let receipt = bind_delegation_task(&resources, &session.thread_id(), &invocation, "task")
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    assert_eq!(resources.writes.load(Ordering::SeqCst), 1);
    assert_eq!(resources.resolutions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn conflicting_binding_receipt_is_not_accepted() {
    for mode in [
        AckMode::ConflictingReceipt,
        AckMode::ConflictingResolvedReceipt,
    ] {
        let session = TestSession::open().await;
        let invocation = prepare_invocations(&session, 1).await.remove(0);
        let resources = BindingResources::new(session.resources(), mode);
        let error = bind_delegation_task(&resources, &session.thread_id(), &invocation, "task")
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Incomplete: delegation binding receipt conflicts"
        );
        assert_eq!(resources.writes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn other_parent_writers_do_not_invalidate_observed_binding_revision() {
    let session = TestSession::open().await;
    let invocation = prepare_invocations(&session, 1).await.remove(0);
    let resources = BindingResources::new(session.resources(), AckMode::ConcurrentWriter);
    let receipt = bind_delegation_task(&resources, &session.thread_id(), &invocation, "task")
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    assert_eq!(resources.writes.load(Ordering::SeqCst), 1);
    assert_eq!(resources.resolutions.load(Ordering::SeqCst), 0);
    let effect = binding_effect(
        session.resources().as_ref(),
        &session.thread_id(),
        &invocation.intent.invocation_id,
    )
    .await;
    assert_eq!(effect.binding.unwrap().owner_task_id, "task");
}

#[tokio::test]
async fn binding_ack_resolution_preserves_original_identity_without_serializing_other_invocations()
{
    let session = TestSession::open().await;
    let invocations = prepare_invocations(&session, 2).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut unresolved = BindingResources::new(session.resources(), AckMode::Unknown);
    unresolved.resolution_pause = Some((entered.clone(), release.clone()));
    let first_id = session.thread_id();
    let first_invocation = invocations[0].clone();
    let first = tokio::spawn(async move {
        bind_delegation_task(&unresolved, &first_id, &first_invocation, "first-task").await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let normal = BindingResources::new(session.resources(), AckMode::Normal);
    let receipt = tokio::time::timeout(
        Duration::from_secs(5),
        bind_delegation_task(
            &normal,
            &session.thread_id(),
            &invocations[1],
            "second-task",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    release.notify_one();
    assert!(first
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("ACK unknown"));
}

#[tokio::test]
async fn conflicting_immutable_binding_is_a_typed_explicit_rejection_without_retry() {
    let session = TestSession::open().await;
    let invocation = prepare_invocations(&session, 1).await.remove(0);
    let resources = BindingResources::new(session.resources(), AckMode::Normal);
    bind_delegation_task(
        &resources,
        &session.thread_id(),
        &invocation,
        "original-task",
    )
    .await
    .unwrap();
    let error = bind_delegation_task(&resources, &session.thread_id(), &invocation, "other-task")
        .await
        .unwrap_err();
    let typed = error
        .downcast_ref::<crate::tools::EffectiveToolError>()
        .unwrap();
    assert_eq!(
        typed.code,
        crate::tools::EffectiveToolErrorCode::ApplicationFailed
    );
    assert!(error.to_string().contains("Conflict"));
    assert_eq!(resources.writes.load(Ordering::SeqCst), 2);
    assert_eq!(resources.resolutions.load(Ordering::SeqCst), 0);
    let effect = binding_effect(
        session.resources().as_ref(),
        &session.thread_id(),
        &invocation.intent.invocation_id,
    )
    .await;
    assert_eq!(effect.binding.unwrap().owner_task_id, "original-task");
}
