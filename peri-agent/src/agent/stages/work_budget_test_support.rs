use std::sync::Arc;

use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::*;
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use tokio::sync::Mutex;

use super::super::super::work_ledger::WorkMutationBarrier;
use super::super::super::work_pipeline::WorkSession;
use super::super::super::StageContext;

struct BudgetResources {
    facts: Mutex<WorkFacts>,
    resources: Arc<dyn SessionResources>,
}

macro_rules! budget_resources {
    ($(fn $method:ident($($argument:ident: $argument_type:ty),*) -> $result:ty;)*) => {
        #[async_trait::async_trait]
        impl SessionResources for BudgetResources {
async fn inspect_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkInspection> {
    let facts = self.facts.lock().await;
    assert_eq!(query.session_id, facts.session_id);
    let page = match &query.selector {
        WorkSelector::Head => WorkPage::Head,
        WorkSelector::Processing { processing_id } => WorkPage::Processings(facts.processing.iter()
            .filter(|record| &record.processing_id == processing_id).cloned().collect()),
        WorkSelector::ProcessingDeliveries { processing_id } => WorkPage::Deliveries(facts.deliveries.iter()
            .filter(|record| record.processing_id.as_ref() == Some(processing_id)).cloned().collect()),
        WorkSelector::Effects { processing_id, phase_sequence } => WorkPage::Effects(facts.effects.iter()
            .filter(|record| record.processing_id.as_ref() == Some(processing_id)
                && phase_sequence.is_none_or(|phase| record.phase_sequence == phase)).cloned().collect()),
        WorkSelector::Effect { invocation_id } => WorkPage::Effects(facts.effects.iter()
            .filter(|record| &record.invocation_id == invocation_id).cloned().collect()),
        WorkSelector::PendingCommands => WorkPage::Commands(Vec::new()),
        WorkSelector::Admission { admission_id } => WorkPage::Admissions(facts.admission.iter()
            .filter(|record| &record.admission.admission_id == admission_id).cloned().collect()),
        _ => return Err(SessionResourceError::conflict("unsupported bounded budget fixture query")),
    };
    Ok(WorkInspection { session_id: facts.session_id.clone(), control: facts.control.clone(),
        head: facts.head.clone(), page, next_cursor: None })
}

async fn prepare_evidence(&self, evidence: &EvidenceWrite) -> SessionResourceResult<PayloadRef> {
    self.resources.prepare_evidence(evidence).await
}

async fn read_evidence(&self, query: &EvidenceQuery) -> SessionResourceResult<EvidenceRecord> {
    self.resources.read_evidence(query).await
}

async fn load_session_control(&self, id: &ThreadId) -> SessionResourceResult<ControlState> {
    let facts = self.facts.lock().await;
    assert_eq!(id, &facts.session_id);
    Ok(facts.control.clone())
}

async fn apply_work_mutation(&self, command: &WorkCommand) -> SessionResourceResult<WorkReceipt> {
    let mut facts = self.facts.lock().await;
    let transition = transition_work(command, &facts)?;
    for write in transition.writes {
        match write {
            WorkWrite::Head { record, .. } => facts.head = record,
            WorkWrite::Processing { record, .. } => facts.processing = Some(record),
            WorkWrite::Effect { record, .. } => {
                if let Some(prior) = facts.effects.iter_mut().find(|prior| prior.invocation_id == record.invocation_id) {
                    *prior = record;
                } else { facts.effects.push(record); }
            }
            WorkWrite::Delivery { record, .. } => {
                if let Some(prior) = facts.deliveries.iter_mut().find(|prior| prior.delivery_id == record.delivery_id) {
                    *prior = record;
                } else { facts.deliveries.push(record); }
            }
            WorkWrite::Control { record, .. } => facts.control = record,
            WorkWrite::Transcript { payload } => {
                let evidence = self.resources.read_evidence(&EvidenceQuery {
                    session_id: command.session_id.clone(), reference: payload.content,
                }).await?;
                let persisted = peri_acp_types::store::deserialize_persisted_payload(
                    std::str::from_utf8(&evidence.bytes).unwrap()).unwrap();
                self.resources.append_history(&command.session_id, &[persisted]).await?;
            }
            _ => return Err(SessionResourceError::conflict("unexpected budget fixture transition")),
        }
    }
    Ok(transition.receipt)
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
let snapshot = session.inspect_head().await.unwrap();
let processing = session.processing(&session.admission.work_id).await.unwrap();
let deliveries = session.deliveries(&processing.processing_id).await.unwrap();
let effects = session.effects(&processing).await.unwrap();
let mut head = snapshot.head;
head.limits.reason_requests = reason_requests;
head.limits.dispatches = dispatches;
let admission = crate::session::work_access::admission(session.ledger.resources().as_ref(), &session.admission).await.unwrap();
let resources: Arc<dyn SessionResources> = Arc::new(BudgetResources {
    resources: session.ledger.resources(),
    facts: Mutex::new(WorkFacts { session_id: session.admission.session_id.clone(),
        control: snapshot.control, head, processing: Some(processing), deliveries, effects,
        drafts: Vec::new(), admission: Some(admission), recovery_descriptor: None,
        terminal_obligation: None, legacy_evidence: None, parent_binding_receipt: None, parent_effect: None }),
});
    let session_id = session.admission.session_id.clone();
    context.work.state.lock().await.session = Some(Arc::new(WorkSession {
        admission: session.admission.clone(),
        ledger: WorkMutationBarrier::new(Arc::clone(&resources)),
    }));
    let mut transcript = context.session.transcript.write();
    *transcript = std::mem::take(&mut *transcript).with_persistence(resources, session_id);
}
