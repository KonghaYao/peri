use std::sync::Arc;

use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{work::*, *};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};

pub(super) struct UncertainWorkResources(pub(super) Arc<dyn SessionResources>);

macro_rules! uncertain_work_resources {
    ($(fn $method:ident($($argument:ident: $argument_type:ty),*) -> $result:ty;)*) => {
        #[async_trait::async_trait]
        impl SessionResources for UncertainWorkResources {
            async fn apply_work_mutation(&self, command: &WorkCommand) -> SessionResourceResult<WorkReceipt> {
                Err(SessionResourceError::persistence_uncertain(Some(command.session_id.clone())))
            }

            async fn resolve_work_mutation(&self, _: &WorkCommand) -> SessionResourceResult<WorkResolution> {
                Ok(WorkResolution::Unknown)
            }

            $(async fn $method(&self, $($argument: $argument_type),*) -> $result {
                self.0.$method($($argument),*).await
            })*
        }
    };
}

uncertain_work_resources! {
    fn inspect_work(query: &WorkQuery) -> SessionResourceResult<WorkInspection>;
    fn prepare_evidence(evidence: &EvidenceWrite) -> SessionResourceResult<PayloadRef>;
    fn read_evidence(query: &EvidenceQuery) -> SessionResourceResult<EvidenceRecord>;
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
