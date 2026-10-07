use std::sync::Arc;

use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::*;
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use tokio::sync::Mutex;

use super::work_ledger::WorkMutationBarrier;
use super::work_pipeline::WorkSession;
use super::StageContext;

struct BudgetResources {
    snapshot: Mutex<WorkSnapshot>,
}

macro_rules! budget_resources {
    ($(fn $method:ident($($argument:ident: $argument_type:ty),*) -> $result:ty;)*) => {
        #[async_trait::async_trait]
        impl SessionResources for BudgetResources {
            async fn load_session_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkSnapshot> {
                let saved = self.snapshot.lock().await;
                assert_eq!(query.session_id, saved.session_id);
                Ok(WorkSnapshot::from_state(query, saved.control.clone(), saved.state.clone()))
            }

            async fn load_session_control(&self, id: &ThreadId) -> SessionResourceResult<ControlState> {
                let saved = self.snapshot.lock().await;
                assert_eq!(id, &saved.session_id);
                Ok(saved.control.clone())
            }

            async fn apply_work_mutation(&self, command: &peri_acp_types::session_resources::work::PreparedWorkCommand) -> SessionResourceResult<WorkReceipt> {
                let mut saved = self.snapshot.lock().await;
                assert_eq!(command.session_id, saved.session_id);
                // Mirrors `SqliteSessionData::write_work`: a rejected reduction
                // leaves the persisted state untouched.
                let reduction = reduce_work(command, &saved.control, saved.state.clone())?;
                if let Some(state) = reduction.state {
                    saved.state = state;
                }
                if let Some(control) = reduction.control {
                    saved.control = control;
                }
                Ok(reduction.receipt)
            }

            $(async fn $method(&self, $($argument: $argument_type),*) -> $result {
                $(let _ = $argument;)*
                panic!("budget fixture does not implement {}", stringify!($method))
            })*
        }
    };
}

budget_resources! {
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

pub(super) async fn install(context: &StageContext, reason_requests: u64, dispatches: u64) {
    let session = context.work.ensure(context).await.unwrap().unwrap();
    let mut snapshot = session.snapshot().await.unwrap();
    snapshot.state.limits.reason_requests = reason_requests;
    snapshot.state.limits.dispatches = dispatches;
    let resources: Arc<dyn SessionResources> = Arc::new(BudgetResources {
        snapshot: Mutex::new(snapshot),
    });
    let session_id = session.admission.session_id.clone();
    context.work.state.lock().await.session = Some(Arc::new(WorkSession {
        admission: session.admission.clone(),
        ledger: WorkMutationBarrier::new(Arc::clone(&resources)),
    }));
    let mut transcript = context.session.transcript.write();
    *transcript = std::mem::take(&mut *transcript).with_persistence(resources, session_id);
}
