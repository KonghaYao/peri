use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, ExecutionAdmissionPort,
};
use peri_acp_types::session_resources::work::*;

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
            if let (Some(session), Some(work_id)) = (&state.session, &state.work_id) {
                let snapshot = session.inspect_head().await?;
                if let Some(error) = super::work_reason::blocked_budget_error(
                    &session.processing(work_id).await?,
                    &snapshot.head.limits,
                ) {
                    return Err(anyhow::Error::new(error));
                }
            }
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
                .inspect_work(&WorkQuery::new(session_id, WorkSelector::Availability))
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
        let snapshot = session.inspect_head().await?;
        let processing = session.ledger.inspect(&WorkQuery::new(&session.admission.session_id,
            WorkSelector::Processing { processing_id: session.admission.work_id.clone() })).await?;
        if let WorkPage::Processings(records) = processing.page {
            if let Some(processing) = records.first() {
                if let Some(error) = super::work_reason::blocked_budget_error(processing, &snapshot.head.limits) {
                    return Err(error.into());
                }
            }
        }
        let pending = session
            .ledger
            .inspect(&WorkQuery::new(
                &session.admission.session_id,
                WorkSelector::PendingCommands,
            ))
            .await?;
        if matches!(pending.page, WorkPage::Commands(ref records) if !records.is_empty()) {
            return Err(anyhow::anyhow!(
                "durable work is blocked or has unresolved original commands"
            ));
        }
        let registration = session
            .ledger
            .inspect(&WorkQuery::new(
                &session.admission.session_id,
                WorkSelector::Admission {
                    admission_id: session.admission.admission_id.clone(),
                },
            ))
            .await?;
        let WorkPage::Admissions(records) = registration.page else {
            return Err(anyhow::anyhow!("admission query returned a different page"));
        };
        let registration = records.first().filter(|record| {
            record.admission == session.admission
                && !record.entering_mutation_id.is_empty()
                && record.leaving_evidence_id.is_none()
        });
        let registered = registration.is_some();
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
        let entry_evidence_id = registration
            .expect("validated admission")
            .entering_mutation_id
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

    pub(crate) async fn settlement_session(
        &self,
        ctx: &StageContext,
    ) -> anyhow::Result<Option<Arc<WorkSession>>> {
        if let Some(session) = self.state.lock().await.session.clone() {
            return Ok(Some(session));
        }
        self.ensure(ctx).await
    }
}

impl WorkSession {
    pub(crate) fn target(processing: &Processing) -> WorkTarget {
        WorkTarget {
            work_id: processing.processing_id.clone(),
            expected_work_revision: processing.revision,
        }
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
