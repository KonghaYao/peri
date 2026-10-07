use super::*;
use crate::transport::RequestTransport;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement, ControlState,
    ForkSnapshot, FrozenSnapshotBytes, NewSession, NewSessionDraft, PersistenceRecovery,
    RewindBoundary, SessionAvailability, SessionInitialization, SessionMetaPatch,
    SessionResourceResult, SessionResources, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct BlockedWorkQuery {
    inner: Arc<dyn SessionResources>,
    entered: Notify,
    release: Notify,
    pause_next: AtomicBool,
}

macro_rules! forward_session_resources {
    ($( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*) => {
        #[async_trait]
        impl SessionResources for BlockedWorkQuery {
            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                self.inner.$method($( $argument ),*).await
            })*

            async fn load_session_control(&self, id: &ThreadId) -> SessionResourceResult<ControlState> {
                self.inner.load_session_control(id).await
            }

            async fn load_session_work(
                &self,
                query: &peri_acp_types::session_resources::work::WorkQuery,
            ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkSnapshot> {
                if self.pause_next.swap(false, Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                self.inner.load_session_work(query).await
            }
        }
    };
}

forward_session_resources! {
    inspect_availability(session: Option<&ThreadId>) -> SessionAvailability;
    resolve_workspace(cwd: &std::path::Path) -> ResolvedWorkspace;
    validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> ();
    finish_close(id: &ThreadId) -> ();
    close_settlement(id: &ThreadId) -> CloseSettlement;
    create_session(input: &NewSession) -> ();
    abandon_initialization(id: &ThreadId) -> ();
    begin_initialization(draft: &NewSessionDraft) -> Arc<dyn SessionInitialization>;
    discard_incomplete_initialization(id: &ThreadId) -> ();
    adopt_legacy_session(id: &ThreadId, saved_cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> ();
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_history(id: &ThreadId) -> Vec<PersistedPayload>;
    load_session_meta(id: &ThreadId) -> ThreadMeta;
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> bool;
    mark_session_closing(id: &ThreadId) -> ();
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary) -> ();
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}

#[tokio::test]
async fn blocked_work_query_does_not_hold_sessions_or_block_input_snapshot() {
    let temporary = tempfile::TempDir::new().unwrap();
    let (mut cfg, states, session_id) = make_user_input_session(&temporary).await;
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let client = Arc::new(client);
    let server: Arc<dyn crate::transport::AcpTransport> = Arc::new(server);
    let mailbox = crate::host::user_input::ensure_mailbox(&session_id, &cfg, &server)
        .await
        .unwrap();
    let blocked = Arc::new(BlockedWorkQuery {
        inner: Arc::clone(&cfg.session_resources),
        entered: Notify::new(),
        release: Notify::new(),
        pause_next: AtomicBool::new(true),
    });
    cfg.session_resources = blocked.clone();
    let cfg = Arc::new(cfg);
    let states = Arc::new(tokio::sync::Mutex::new(states));
    let observed_states = Arc::clone(&states);
    let task = tokio::spawn(async move {
        let (continuation, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let continuation = Arc::new(continuation);
        let prompt_locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let connection = Arc::new(tokio::sync::Mutex::new(
            crate::host::connection::ConnectionContext::new(false),
        ));
        let cancellation = CancellationToken::new();
        crate::host::server_loop::ServerLoop {
            transport: &server,
            cfg: &cfg,
            sessions: &states,
            prompt_locks: &prompt_locks,
            cont_tx: &continuation,
            connection: &connection,
            connection_cancellation: &cancellation,
        }
        .run()
        .await;
    });
    let querying_client = Arc::clone(&client);
    let querying_session = session_id.clone();
    let query = tokio::spawn(async move {
        querying_client
            .send_request("session/work/query", json!({"sessionId": querying_session}))
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        blocked.entered.notified(),
    )
    .await
    .unwrap();
    assert!(observed_states.try_lock().is_ok());
    assert!(!query.is_finished());
    let snapshot = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.send_request("session/input/snapshot", json!({"sessionId": session_id})),
    )
    .await
    .expect("snapshot must progress while work query is suspended")
    .unwrap();
    assert_eq!(snapshot["generation"], mailbox.generation());
    assert!(!query.is_finished());
    blocked.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), query)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    client.close();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn session_io_preparation_rejects_mutations_during_close_but_allows_snapshot() {
    let temporary = tempfile::TempDir::new().unwrap();
    let (_cfg, mut states, session_id) = make_user_input_session(&temporary).await;
    states.get_mut(&session_id).unwrap().closing = true;
    let params = json!({"sessionId": session_id});
    let error =
        crate::host::requests::session_io::prepare("session/input/enqueue", &params, &states)
            .err()
            .unwrap();
    assert_eq!(error.code, -32010);
    assert!(
        crate::host::requests::session_io::prepare("session/input/snapshot", &params, &states)
            .is_ok()
    );
    assert!(crate::host::requests::session_io::prepare(
        "session/input/enqueue",
        &json!({"sessionId": "missing"}),
        &states
    )
    .is_err());
}
