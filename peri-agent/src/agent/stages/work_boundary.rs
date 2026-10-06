use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, ExecutionAdmissionPort,
};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::store::{deserialize_persisted_payload, PersistedPayload};
use tokio::sync::Mutex;

use super::work_pipeline::{WorkMode, WorkRuntime, WorkSession};
use super::{StageContext, StageContextBuilder};
use crate::tools::CanonicalToolInvocation;

pub type SdkRunStartedFn = Arc<
    dyn Fn(
            WorkAdmission,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub type SdkAdmissionObservedFn = Arc<dyn Fn(WorkAdmission) + Send + Sync>;

pub(crate) struct BoundaryState {
    pub(crate) frozen: bool,
    pub(crate) session: Option<Arc<WorkSession>>,
    pub(crate) work_id: Option<String>,
    pub(crate) request_id: Option<String>,
    pub(crate) invocations: HashMap<String, CanonicalToolInvocation>,
}

pub(crate) struct WorkBoundary {
    mode: WorkMode,
    pub(crate) state: Mutex<BoundaryState>,
    admission_request_id: String,
}

impl Default for WorkBoundary {
    fn default() -> Self {
        Self::new(WorkMode::Required)
    }
}

impl WorkBoundary {
    fn new(mode: WorkMode) -> Self {
        Self {
            mode,
            state: Mutex::new(BoundaryState {
                frozen: false,
                session: None,
                work_id: None,
                request_id: None,
                invocations: HashMap::new(),
            }),
            admission_request_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self::new(WorkMode::BestEffortFixture)
    }

    #[cfg(test)]
    pub(crate) fn is_best_effort_fixture(&self) -> bool {
        self.mode == WorkMode::BestEffortFixture
    }

    pub(crate) async fn ensure(
        &self,
        ctx: &StageContext,
    ) -> anyhow::Result<Option<Arc<WorkSession>>> {
        #[cfg(test)]
        if self.mode == WorkMode::BestEffortFixture {
            return Ok(None);
        }
        let mut state = self.state.lock().await;
        if state.frozen {
            return Err(anyhow::anyhow!(
                "durable execution is frozen for reconciliation"
            ));
        }
        if let Some(session) = &state.session {
            if session.ledger.pending_command().await.is_some() {
                return Err(anyhow::anyhow!(
                    "work mutation remains unconfirmed; resolve original command before effects"
                ));
            }
            return Ok(Some(Arc::clone(session)));
        }
        let port = ctx.session.transcript.read().idempotent_reminder_port();
        let resources = port.as_ref().map(|(resources, _, _)| Arc::clone(resources));
        if ctx.session.turn.work_admission().is_none() {
            let admission_port = ctx.execution_admission_port().ok_or_else(|| {
                anyhow::anyhow!("required execution has no SDK ticket or admission port")
            })?;
            let (resources, session_id, _) = port.as_ref().ok_or_else(|| {
                anyhow::anyhow!("required execution has no durable SessionResources port")
            })?;
            let lifecycle = ctx
                .recipient_lifecycle
                .ok_or_else(|| anyhow::anyhow!("owned recipient lifecycle is missing"))?;
            super::work_receive::publish_session_inbox(
                Arc::clone(resources),
                session_id,
                lifecycle,
                &ctx.session.queue,
            )
            .await?;
            let snapshot = resources
                .load_session_work(&WorkQuery {
                    session_id: session_id.clone(),
                    limit: 1,
                })
                .await?;
            let outcome = admission_port
                .admit(AdmissionRequest {
                    request_id: self.admission_request_id.clone(),
                    snapshot: snapshot.into(),
                    existing_admission: None,
                })
                .await
                .map_err(|error| anyhow::anyhow!(error))?;
            let AdmissionOutcome::Admitted { admission } = outcome else {
                return Err(anyhow::anyhow!(
                    "SDK execution admission not confirmed: {outcome:?}"
                ));
            };
            if admission.session_id != *session_id || admission.lifecycle != lifecycle {
                return Err(anyhow::anyhow!(
                    "SDK admission belongs to a different recipient"
                ));
            }
            let command = WorkCommand {
                session_id: session_id.clone(),
                recipient_lifecycle: lifecycle,
                mutation_id: format!("register-admission:{}", admission.admission_id),
                action: WorkAction::RegisterAdmission {
                    admission: admission.clone(),
                },
            };
            super::work_ledger::WorkMutationBarrier::new(Arc::clone(resources))
                .commit(&command)
                .await?;
            if !ctx.session.turn.bind_work_admission(admission) {
                return Err(anyhow::anyhow!("SDK execution ticket binding changed"));
            }
        }
        let runtime = WorkRuntime::bind(self.mode, ctx.session.turn.work_admission(), resources)?;
        #[cfg(not(test))]
        let WorkRuntime::Durable(session) = runtime;
        #[cfg(test)]
        let session = match runtime {
            WorkRuntime::Durable(session) => session,
            WorkRuntime::BestEffortFixture => return Ok(None),
        };
        let snapshot = session.snapshot().await?;
        if snapshot.blocked || !snapshot.pending_commands.is_empty() {
            return Err(anyhow::anyhow!(
                "durable work is blocked or has unresolved original commands"
            ));
        }
        let registered = snapshot
            .state
            .admissions
            .get(&session.admission.admission_id)
            .is_some_and(|record| {
                record.admission == session.admission
                    && record.entering_receipt.is_some()
                    && record.settled_receipt.is_none()
            });
        if !registered
            || port
                .as_ref()
                .is_none_or(|(_, session_id, _)| *session_id != session.admission.session_id)
        {
            return Err(anyhow::anyhow!(
                "SDK ticket is not durably registered for this session"
            ));
        }
        if snapshot.control.lifecycle != session.admission.lifecycle
            || snapshot.control.control_generation != session.admission.control_generation
            || snapshot.control.attempt.as_ref() != Some(&session.admission.execution)
            || !ctx
                .session
                .turn
                .bind_control_generation(session.admission.control_generation)
        {
            return Err(anyhow::anyhow!(
                "SDK ticket no longer matches exact execution control"
            ));
        }
        if let Some(observe) = &ctx.sdk_admission_observed {
            observe(session.admission.clone());
        }
        let entry_port = ctx
            .execution_admission_port()
            .ok_or_else(|| anyhow::anyhow!("SDK entered-ACK port missing"))?;
        let entry_evidence_id = snapshot.state.admissions[&session.admission.admission_id]
            .entering_receipt
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("durable registration receipt missing"))?
            .mutation_id
            .clone();
        let entry = entry_port
            .entered(peri_acp_types::execution_admission::EntryRequest {
                admission: session.admission.clone(),
                entry_evidence_id: entry_evidence_id.clone(),
            })
            .await
            .map_err(anyhow::Error::new)?;
        if !matches!(&entry, peri_acp_types::execution_admission::EntryOutcome::Applied { receipt }
            if receipt.admission == session.admission && receipt.entry_evidence_id == entry_evidence_id)
        {
            return Err(anyhow::anyhow!(
                "SDK execution entered-ACK unconfirmed: {entry:?}"
            ));
        }
        if ctx
            .recipient_lifecycle
            .is_some_and(|lifecycle| lifecycle != session.admission.lifecycle)
        {
            return Err(anyhow::anyhow!(
                "SDK ticket differs from owned recipient lifecycle"
            ));
        }
        if let Some((pool, manager)) = &ctx.mcp_work_binding {
            pool.bind_agent_session_for_lifecycle(
                &session.admission.session_id,
                session.admission.lifecycle,
                peri_acp_types::session::SessionInbox::new(Arc::new(ctx.session.queue.clone()))
                    .handle(),
                Arc::clone(manager),
            )
            .map_err(anyhow::Error::msg)?;
            pool.bind_agent_session_resources(
                &session.admission.session_id,
                session.admission.lifecycle,
                session.ledger.resources(),
            )
            .map_err(anyhow::Error::msg)?;
        }
        if let Some(mailbox) = &ctx.session.user_input_mailbox {
            mailbox
                .observe_sdk_run(&session.admission)
                .await
                .map_err(anyhow::Error::new)?;
            if !mailbox.attach_sdk_attempt(
                &session.admission,
                ctx.session.turn.cancel_token.as_ref().clone(),
            ) {
                return Err(anyhow::anyhow!(
                    "SDK run observer could not attach the exact executing attempt"
                ));
            }
            let started = ctx
                .sdk_run_started
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("SDK RunStarted publication barrier missing"))?;
            started(session.admission.clone())
                .await
                .map_err(anyhow::Error::msg)?;
        }
        state.work_id = Some(session.admission.work_id.clone());
        state.session = Some(Arc::clone(&session));
        Ok(Some(session))
    }
}

impl WorkSession {
    pub(crate) async fn snapshot(&self) -> anyhow::Result<WorkSnapshot> {
        Ok(self
            .ledger
            .snapshot(&WorkQuery {
                session_id: self.admission.session_id.clone(),
                limit: 1,
            })
            .await?)
    }

    pub(crate) fn target(snapshot: &WorkSnapshot, work_id: &str) -> anyhow::Result<WorkTarget> {
        let work = snapshot
            .state
            .works
            .get(work_id)
            .ok_or_else(|| anyhow::anyhow!("durable stage work is missing"))?;
        Ok(WorkTarget {
            work_id: work.work_id.clone(),
            expected_work_revision: work.revision,
        })
    }
}

impl StageContext {
    #[cfg(test)]
    pub fn best_effort_fixture_builder(
        turn: crate::session::turn::TurnContext,
        transcript: Arc<parking_lot::RwLock<crate::session::MessageTranscript>>,
        queue: crate::session::MessageQueue,
    ) -> StageContextBuilder {
        Self::builder(turn, transcript, queue).with_best_effort_work_fixture()
    }
    pub fn execution_admission_port(&self) -> Option<Arc<dyn ExecutionAdmissionPort>> {
        self.execution_admission_port.clone()
    }

    pub fn with_sdk_admission_observed(mut self, observe: SdkAdmissionObservedFn) -> Self {
        self.sdk_admission_observed = Some(observe);
        self
    }
}

impl StageContextBuilder {
    pub fn with_sdk_admission_observed(mut self, observe: SdkAdmissionObservedFn) -> Self {
        self.sdk_admission_observed = Some(observe);
        self
    }
    pub fn with_sdk_run_started(mut self, publish: SdkRunStartedFn) -> Self {
        self.sdk_run_started = Some(publish);
        self
    }
    pub fn with_recipient_lifecycle(mut self, lifecycle: u64) -> Self {
        self.recipient_lifecycle = Some(lifecycle);
        self
    }

    pub fn with_work_mcp_binding(
        mut self,
        pool: Arc<dyn peri_acp_types::ports::McpPoolPort>,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) -> Self {
        self.mcp_work_binding = Some((pool, manager));
        self
    }
    pub fn with_execution_admission_port(mut self, port: Arc<dyn ExecutionAdmissionPort>) -> Self {
        self.execution_admission_port = Some(port);
        self
    }

    #[cfg(test)]
    pub fn with_best_effort_work_fixture(mut self) -> Self {
        self.work = Arc::new(WorkBoundary::fixture());
        self
    }
}

pub(crate) fn persisted_projection(payload: &WorkPayload) -> anyhow::Result<PersistedPayload> {
    payload.validate()?;
    deserialize_persisted_payload(&payload.serialized)
}
