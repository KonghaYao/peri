use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::session::MessagePolicy;
use peri_acp_types::session_resources::*;
use peri_acp_types::store::{CompactionChange, MessageFlags};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy)]
enum Failure {
    None,
    Read,
    NotApplied,
    Unknown,
}

struct PublicationResources {
    backend: Arc<dyn SessionResources>,
    queries: Mutex<Vec<WorkQuery>>,
    commands: Mutex<Vec<WorkCommand>>,
    snapshots: AtomicUsize,
    resolutions: AtomicUsize,
    failure: Mutex<Failure>,
}

impl PublicationResources {
    fn new(backend: Arc<dyn SessionResources>) -> Self {
        Self {
            backend,
            queries: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
            snapshots: AtomicUsize::new(0),
            resolutions: AtomicUsize::new(0),
            failure: Mutex::new(Failure::None),
        }
    }
}

macro_rules! publication_resources {
    ($(fn $method:ident($($argument:ident: $argument_type:ty),*) -> $result:ty;)*) => {
        #[async_trait::async_trait]
        impl SessionResources for PublicationResources {
            async fn inspect_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkInspection> {
                if !matches!(query.selector, WorkSelector::Delivery { .. }) {
                    self.snapshots.fetch_add(1, Ordering::SeqCst);
                    return Err(SessionResourceError::conflict("publication must only inspect exact delivery"));
                }
                self.queries.lock().unwrap().push(query.clone());
                let failure = *self.failure.lock().unwrap();
                if matches!(failure, Failure::Read) {
                    return Err(SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                        detail: "injected narrow read failure".into(),
                    }));
                }
                self.backend.inspect_work(query).await
            }

            async fn prepare_evidence(&self, evidence: &EvidenceWrite) -> SessionResourceResult<PayloadRef> {
                self.backend.prepare_evidence(evidence).await
            }

            async fn read_evidence(&self, query: &EvidenceQuery) -> SessionResourceResult<EvidenceRecord> {
                self.backend.read_evidence(query).await
            }

            async fn apply_work_mutation(&self, command: &WorkCommand) -> SessionResourceResult<WorkReceipt> {
                self.commands.lock().unwrap().push(command.clone());
                let failure = *self.failure.lock().unwrap();
                match failure {
                    Failure::NotApplied => Err(SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                        detail: "injected unapplied publication".into(),
                    })),
                    Failure::Unknown => Err(SessionResourceError::persistence_uncertain(Some(command.session_id.clone()))),
                    _ => self.backend.apply_work_mutation(command).await,
                }
            }

            async fn resolve_work_mutation(&self, _: &WorkCommand) -> SessionResourceResult<WorkResolution> {
                self.resolutions.fetch_add(1, Ordering::SeqCst);
                Ok(WorkResolution::Unknown)
            }

            $(async fn $method(&self, $($argument: $argument_type),*) -> $result {
                $(let _ = $argument;)*
                panic!("publication fixture does not implement {}", stringify!($method))
            })*
        }
    };
}

publication_resources! {
    fn inspect_availability(session: Option<&ThreadId>) -> SessionResourceResult<SessionAvailability>;
    fn resolve_workspace(cwd: &std::path::Path) -> SessionResourceResult<ResolvedWorkspace>;
    fn validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> SessionResourceResult<()>;
    fn finish_close(id: &ThreadId) -> SessionResourceResult<()>;
    fn close_settlement(id: &ThreadId) -> SessionResourceResult<CloseSettlement>;
    fn create_session(input: &NewSession) -> SessionResourceResult<()>;
    fn abandon_initialization(id: &ThreadId) -> SessionResourceResult<()>;
    fn begin_initialization(draft: &NewSessionDraft) -> SessionResourceResult<Arc<dyn SessionInitialization>>;
    fn discard_incomplete_initialization(id: &ThreadId) -> SessionResourceResult<()>;
    fn adopt_legacy_session(id: &ThreadId, cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> SessionResourceResult<()>;
    fn load_session_snapshot(id: &ThreadId) -> SessionResourceResult<SessionSnapshot>;
    fn load_session_binding(id: &ThreadId) -> SessionResourceResult<BindingState>;
    fn validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> SessionResourceResult<ResolvedWorkspace>;
    fn load_session_history(id: &ThreadId) -> SessionResourceResult<Vec<PersistedPayload>>;
    fn load_session_meta(id: &ThreadId) -> SessionResourceResult<ThreadMeta>;
    fn list_sessions(query: &ScopedThreadQuery) -> SessionResourceResult<ScopedThreadPage>;
    fn list_children(parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;
    fn list_session_tree(root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;
    fn append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> SessionResourceResult<()>;
    fn append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> SessionResourceResult<bool>;
    fn mark_session_closing(id: &ThreadId) -> SessionResourceResult<()>;
    fn is_session_closing(id: &ThreadId) -> SessionResourceResult<bool>;
    fn save_fork(fork: &ForkSnapshot) -> SessionResourceResult<()>;
    fn save_child(child: &ChildSnapshot) -> SessionResourceResult<()>;
    fn claim_child_resume(child: &ThreadId, root: &ThreadId) -> SessionResourceResult<Box<dyn ChildResumeClaim>>;
    fn apply_compaction(id: &ThreadId, change: &CompactionChange) -> SessionResourceResult<()>;
    fn apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> SessionResourceResult<()>;
    fn rewind_history(id: &ThreadId, boundary: RewindBoundary) -> SessionResourceResult<()>;
    fn remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> SessionResourceResult<()>;
    fn update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> SessionResourceResult<()>;
    fn delete_session_tree(id: &ThreadId) -> SessionResourceResult<()>;
    fn recover_session_persistence(id: &ThreadId) -> SessionResourceResult<PersistenceRecovery>;
    fn drain_persistence(id: &ThreadId) -> SessionResourceResult<()>;
}

#[tokio::test]
async fn new_delivery_commits_once_and_duplicate_never_loads_history_or_rewrites() {
    let bound = TestSession::open().await;
    let resources = Arc::new(PublicationResources::new(bound.resources()));
    let queue = crate::session::MessageQueue::new();
    let mut message = QueuedMessage::prompt(MessageSource::UserInput, BaseMessage::human("input"));
    let delivery_id = MessageId::new();
    message.delivery_id = Some(delivery_id);
    queue.push(message.clone());
    queue.push(message.clone());
    let receipts = publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
        .await
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].decision, WorkDecision::Accepted);
    assert_eq!(
        receipts[0].delivery_id,
        Some(delivery_id.as_uuid().to_string())
    );
    assert!(queue.drain_batch(64).is_empty());
    message.source = MessageSource::SystemInjected;
    queue.push(message);
    assert!(
        publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(queue.drain_batch(64).is_empty());
    assert_eq!(resources.commands.lock().unwrap().len(), 1);
    assert_eq!(resources.snapshots.load(Ordering::SeqCst), 0);
    assert_eq!(resources.resolutions.load(Ordering::SeqCst), 0);
    assert_eq!(
        resources.queries.lock().unwrap().as_slice(),
        &vec![
            WorkQuery::new(
                bound.thread_id.clone(),
                WorkSelector::Delivery {
                    delivery_id: delivery_id.as_uuid().to_string(),
                }
            );
            3
        ]
    );
}

#[tokio::test]
async fn lifecycle_content_and_policy_conflicts_keep_the_whole_queue() {
    let bound = TestSession::open().await;
    let resources = Arc::new(PublicationResources::new(bound.resources()));
    let queue = crate::session::MessageQueue::new();
    let mut original =
        QueuedMessage::prompt(MessageSource::UserInput, BaseMessage::human("original"));
    original.delivery_id = Some(MessageId::new());
    queue.push(original.clone());
    publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
        .await
        .unwrap();
    for conflict in ["lifecycle", "content", "policy"] {
        let mut message = original.clone();
        let lifecycle = if conflict == "lifecycle" { 2 } else { 1 };
        if conflict == "content" {
            message.payload = QueuedPayload::Message(BaseMessage::human("conflicting content"));
        }
        if conflict == "policy" {
            message.policy = MessagePolicy::passive();
        }
        queue.push(message.clone());
        queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human("tail"),
        ));
        assert!(
            publish_session_inbox(resources.clone(), &bound.thread_id, lifecycle, &queue)
                .await
                .is_err()
        );
        let retained = queue.drain_batch(64);
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].delivery_id, original.delivery_id);
        assert_eq!(retained[0].policy, message.policy);
        let QueuedPayload::Message(expected) = &message.payload else {
            panic!("message payload")
        };
        let QueuedPayload::Message(actual) = &retained[0].payload else {
            panic!("message payload")
        };
        assert_eq!(actual.id(), expected.id());
        assert!(retained[1].delivery_id.is_some());
        assert!(retained[0].admission_sequence < retained[1].admission_sequence);
    }
    assert_eq!(resources.commands.lock().unwrap().len(), 1);
    assert_eq!(resources.snapshots.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn not_applied_retains_generated_identity_and_retries_the_same_mutation() {
    let bound = TestSession::open().await;
    let resources = Arc::new(PublicationResources::new(bound.resources()));
    *resources.failure.lock().unwrap() = Failure::NotApplied;
    let queue = crate::session::MessageQueue::new();
    let payload = BaseMessage::human("retry");
    let payload_id = payload.id();
    queue.push(QueuedMessage::prompt(MessageSource::UserInput, payload));
    assert!(
        publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
            .await
            .is_err()
    );
    let retained = queue.drain_batch(64);
    assert_eq!(retained.len(), 1);
    let delivery_id = retained[0].delivery_id.unwrap();
    assert_ne!(delivery_id, payload_id);
    let original = resources.commands.lock().unwrap()[0].clone();
    assert_eq!(
        original.mutation_id,
        format!(
            "inbox-publish:{}:1:{}",
            bound.thread_id,
            delivery_id.as_uuid()
        )
    );
    queue.push_batch(retained);
    *resources.failure.lock().unwrap() = Failure::None;
    assert_eq!(
        publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        resources.commands.lock().unwrap().as_slice(),
        &[original.clone(), original]
    );
    assert_eq!(resources.resolutions.load(Ordering::SeqCst), 0);
    assert_eq!(resources.snapshots.load(Ordering::SeqCst), 0);
    assert!(queue.drain_batch(64).is_empty());
}

#[tokio::test]
async fn unknown_keeps_identity_without_replay_and_late_publication_is_deduplicated() {
    let bound = TestSession::open().await;
    let resources = Arc::new(PublicationResources::new(bound.resources()));
    *resources.failure.lock().unwrap() = Failure::Unknown;
    let queue = crate::session::MessageQueue::new();
    queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("uncertain"),
    ));
    let error = publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<super::super::work_ledger::WorkCommitError>(),
        Some(super::super::work_ledger::WorkCommitError::Unknown { .. })
    ));
    assert_eq!(resources.commands.lock().unwrap().len(), 1);
    assert_eq!(resources.resolutions.load(Ordering::SeqCst), 1);
    let retained = queue.drain_batch(64);
    assert_eq!(retained.len(), 1);
    let original = resources.commands.lock().unwrap()[0].clone();
    assert_eq!(
        original.mutation_id,
        format!(
            "inbox-publish:{}:1:{}",
            bound.thread_id,
            retained[0].delivery_id.unwrap().as_uuid()
        )
    );
    bound
        .resources
        .apply_work_mutation(&original)
        .await
        .unwrap();
    queue.push_batch(retained);
    *resources.failure.lock().unwrap() = Failure::None;
    assert!(
        publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(resources.commands.lock().unwrap().len(), 1);
    assert_eq!(resources.snapshots.load(Ordering::SeqCst), 0);
    assert!(queue.drain_batch(64).is_empty());
}

#[tokio::test]
async fn narrow_read_failure_preserves_order_and_all_messages_beyond_batch_limit() {
    let bound = TestSession::open().await;
    let resources = Arc::new(PublicationResources::new(bound.resources()));
    *resources.failure.lock().unwrap() = Failure::Read;
    let queue = crate::session::MessageQueue::new();
    let mut identities = Vec::new();
    for index in 0..65 {
        let mut message = QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(format!("input {index}")),
        );
        let identity = MessageId::new();
        message.delivery_id = Some(identity);
        identities.push(identity);
        queue.push(message);
    }
    assert!(
        publish_session_inbox(resources.clone(), &bound.thread_id, 1, &queue)
            .await
            .is_err()
    );
    let retained = queue.drain_batch(65);
    assert_eq!(
        retained
            .iter()
            .map(|message| message.delivery_id.unwrap())
            .collect::<Vec<_>>(),
        identities
    );
    assert!(retained
        .windows(2)
        .all(|messages| messages[0].admission_sequence < messages[1].admission_sequence));
    assert_eq!(resources.queries.lock().unwrap().len(), 1);
    assert!(resources.commands.lock().unwrap().is_empty());
    assert_eq!(resources.snapshots.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn empty_queue_does_not_touch_store() {
    let resources = Arc::new(PublicationResources::new(
        crate::session::test_resources::mock::MockSessionResources::new(),
    ));
    *resources.failure.lock().unwrap() = Failure::Read;
    let queue = crate::session::MessageQueue::new();
    assert!(
        publish_session_inbox(resources.clone(), "empty-session", 1, &queue)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(resources.queries.lock().unwrap().is_empty());
    assert!(resources.commands.lock().unwrap().is_empty());
    assert_eq!(resources.snapshots.load(Ordering::SeqCst), 0);
    assert_eq!(resources.resolutions.load(Ordering::SeqCst), 0);
}
