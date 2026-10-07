use std::collections::HashSet;
use std::sync::Arc;

use peri_acp_types::execution_admission::{
    AttemptStoppedProof, ExecutionAdmissionPort, SettlementOutcome, SettlementRequest,
};
use peri_acp_types::session_resources::work::{WorkAction, WorkCommand, WorkQuery};
use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, ControlStatus, FrozenState, NewSession, NewSessionMeta,
    SessionResources,
};
use peri_acp_types::store::InheritedContext;
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workflow::AgentRunParams;

use super::WorkflowAgentContext;
use crate::agent::stages::work_ledger::WorkMutationBarrier;
use crate::session::{MessageTranscript, Session, TurnContext};

pub(super) struct WorkflowExecution {
    pub(super) session_id: String,
    pub(super) lifecycle: u64,
    pub(super) resources: Arc<dyn SessionResources>,
    pub(super) admission_port: Arc<dyn ExecutionAdmissionPort>,
}

impl WorkflowExecution {
    pub(super) async fn prepare(
        context: &WorkflowAgentContext,
        params: &AgentRunParams,
    ) -> Result<Self, String> {
        let resources = context
            .session_resources
            .clone()
            .ok_or("Blocked: workflow session resources unavailable")?;
        let admission_port = context
            .execution_admission_port
            .clone()
            .ok_or("Blocked: workflow SDK admission port unavailable")?;
        let parent_id = context
            .session_id
            .as_ref()
            .ok_or("Blocked: workflow initiating session identity unavailable")?;
        let snapshot = resources
            .load_session_snapshot(parent_id)
            .await
            .map_err(|error| error.to_string())?;
        let BindingState::Bound(binding) = snapshot.binding else {
            return Err("Blocked: workflow parent execution binding unavailable".into());
        };
        let FrozenState::Present(frozen) = snapshot.frozen else {
            return Err("Blocked: workflow parent frozen authorization unavailable".into());
        };
        if snapshot.meta.cwd != context.cwd {
            return Err("Blocked: workflow execution workspace differs from parent".into());
        }
        let parent = resources
            .load_session_work(&WorkQuery {
                session_id: parent_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| error.to_string())?;
        if parent.control.status != ControlStatus::Active {
            return Err("Blocked: workflow initiating session is not Active".into());
        }
        let owners = parent
            .state
            .resource_owners
            .get(&parent.control.lifecycle)
            .ok_or("Blocked: workflow owner authorization declarations unavailable")?;
        let mut root_id = parent_id.clone();
        let mut seen = HashSet::new();
        loop {
            if seen.len() >= 128 || !seen.insert(root_id.clone()) {
                return Err("Blocked: workflow parent session ancestry is cyclic".into());
            }
            let ancestor = resources
                .load_session_snapshot(&root_id)
                .await
                .map_err(|error| error.to_string())?;
            match ancestor.meta.parent_thread_id {
                Some(parent) => root_id = parent,
                None => break,
            }
        }
        let session_id = uuid::Uuid::now_v7().to_string();
        resources
            .save_child(&ChildSnapshot {
                target: NewSession {
                    thread_id: session_id.clone(),
                    created_at: peri_time::now_utc_rfc3339(),
                    meta: NewSessionMeta {
                        title: Some(format!("workflow:{}:{}", params.run_id, params.agent_id)),
                        cwd: context.cwd.clone(),
                        parent_thread_id: Some(parent_id.clone()),
                        hidden: true,
                        cancel_policy: CancelPolicy::Cascade,
                        snapshot_at_message_id: None,
                    },
                    binding,
                    frozen,
                },
                parent_id: parent_id.clone(),
                root_id,
                inherited: InheritedContext::default(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let child = resources
            .load_work_availability(&session_id)
            .await
            .map_err(|error| error.to_string())?;
        WorkMutationBarrier::new(resources.clone())
            .commit(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    WorkCommand {
                        session_id: session_id.clone(),
                        recipient_lifecycle: child.control.lifecycle,
                        mutation_id: format!("workflow-child-owners:{session_id}"),
                        action: WorkAction::BindResourceOwners {
                            expected_revision: child.state.revision,
                            connections_json: owners.connections_json.clone(),
                            authorization_ref: owners.authorization_ref.clone(),
                        },
                    },
                )
                .map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(Self {
            session_id,
            lifecycle: child.control.lifecycle,
            resources,
            admission_port,
        })
    }

    pub(super) async fn finish(&self, session: &Session, turn: &TurnContext) -> Result<(), String> {
        let admission = turn
            .work_admission()
            .ok_or("Incomplete: workflow execution has no SDK admission proof")?;
        let flush = session
            .transcript()
            .read()
            .persist_tx_handle()
            .ok_or("Incomplete: workflow transcript persistence barrier unavailable")?;
        MessageTranscript::flush_via_tx(&flush)
            .await
            .map_err(|error| error.to_string())?;
        self.resources
            .drain_persistence(&self.session_id)
            .await
            .map_err(|error| error.to_string())?;
        let control = self
            .resources
            .load_session_control(&self.session_id)
            .await
            .map_err(|error| error.to_string())?;
        if control.attempt.is_some() || control.lifecycle != admission.lifecycle {
            return Err("Incomplete: workflow exact execution exit remains unconfirmed".into());
        }
        self.reconcile_terminal(admission).await?;
        let evidence_id = format!("workflow-execution-exit:{}", admission.admission_id);
        WorkMutationBarrier::new(self.resources.clone())
            .commit(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    WorkCommand {
                        session_id: self.session_id.clone(),
                        recipient_lifecycle: admission.lifecycle,
                        mutation_id: format!(
                            "workflow-execution-finish:{}",
                            admission.admission_id
                        ),
                        action: WorkAction::FinishAdmission {
                            admission: admission.clone(),
                            evidence_id: evidence_id.clone(),
                        },
                    },
                )
                .map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        match self
            .admission_port
            .settle(SettlementRequest {
                admission: admission.clone(),
                proof: AttemptStoppedProof::AttemptStopped {
                    instance_id: admission.instance_id.clone(),
                    generation_id: admission.generation_id.clone(),
                    execution: admission.execution.clone(),
                    evidence_id: evidence_id.clone(),
                },
            })
            .await
            .map_err(|error| error.to_string())?
        {
            SettlementOutcome::Applied { receipt }
                if receipt.admission == *admission && receipt.evidence_id == evidence_id =>
            {
                Ok(())
            }
            _ => Err("Incomplete: workflow SDK settlement remains unconfirmed".into()),
        }
    }

    async fn reconcile_terminal(
        &self,
        admission: &peri_acp_types::session_resources::work::WorkAdmission,
    ) -> Result<(), String> {
        let query = WorkQuery {
            session_id: self.session_id.clone(),
            limit: 1,
        };
        let barrier = WorkMutationBarrier::new(self.resources.clone());
        let mut child = self
            .resources
            .load_session_work(&query)
            .await
            .map_err(|error| error.to_string())?;
        for command in &child.pending_commands {
            if matches!(
                &command.action,
                WorkAction::BindTerminalObligation { admission_id, .. }
                    | WorkAction::AcknowledgeTerminalObligation { admission_id, .. }
                    if admission_id == &admission.admission_id
            ) {
                barrier
                    .commit(
                        &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                            command.clone(),
                        )
                        .map_err(|error| error.to_string())?,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        child = self
            .resources
            .load_session_work(&query)
            .await
            .map_err(|error| error.to_string())?;
        let Some(command) = child
            .state
            .terminal_obligations
            .get(&admission.admission_id)
        else {
            return Ok(());
        };
        let parent_receipt = barrier
            .commit(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    command.clone(),
                )
                .map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        if let Some(receipt) = child
            .state
            .terminal_acknowledgements
            .get(&admission.admission_id)
        {
            return if receipt == &parent_receipt {
                Ok(())
            } else {
                Err("Incomplete: workflow parent terminal acknowledgement conflicts".into())
            };
        }
        let revision = self
            .resources
            .load_work_revision(&self.session_id)
            .await
            .map_err(|error| error.to_string())?;
        barrier
            .commit(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    WorkCommand {
                        session_id: self.session_id.clone(),
                        recipient_lifecycle: admission.lifecycle,
                        mutation_id: format!("workflow-terminal-ack:{}", admission.admission_id),
                        action: WorkAction::AcknowledgeTerminalObligation {
                            expected_revision: revision,
                            admission_id: admission.admission_id.clone(),
                            receipt: parent_receipt,
                        },
                    },
                )
                .map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
