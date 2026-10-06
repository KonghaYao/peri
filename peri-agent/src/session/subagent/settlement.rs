use std::sync::Arc;

use peri_acp_types::execution_admission::{
    AttemptStoppedProof, ExecutionAdmissionPort, SettlementOutcome, SettlementRequest,
};
use peri_acp_types::session_resources::work::{WorkAction, WorkCommand, WorkQuery, WorkSelector};
use peri_acp_types::session_resources::{
    ControlAction, ControlCommand, ControlDecision, SessionResources,
};

use crate::agent::stages::work_ledger::WorkMutationBarrier;
use crate::agent::stages::StageContext;
use crate::session::{MessageTranscript, TurnContext};

pub(super) struct OwnedSubagentExecution {
    turn: Arc<TurnContext>,
    transcript: Arc<parking_lot::RwLock<MessageTranscript>>,
    port: Option<Arc<dyn ExecutionAdmissionPort>>,
}

impl OwnedSubagentExecution {
    pub(super) fn capture(context: &StageContext) -> Self {
        Self {
            turn: context.session.turn.clone(),
            transcript: context.session.transcript.clone(),
            port: context.execution_admission_port.clone(),
        }
    }

    pub(super) async fn prepare_terminal(
        &self,
        result: &peri_acp_types::event::BackgroundTaskResult,
    ) -> Result<(), String> {
        let admission = self
            .turn
            .work_admission()
            .ok_or("Incomplete: child SDK admission unavailable")?;
        let resources = self.resources()?;
        let child = resources
            .inspect_work(&WorkQuery::new(
                admission.session_id.clone(),
                WorkSelector::Head,
            ))
            .await
            .map_err(|error| error.to_string())?;
        if crate::session::work_access::terminal(resources.as_ref(), admission)
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Ok(());
        }
        let binding = crate::session::work_access::delegation(resources.as_ref(), admission)
            .await
            .map_err(|error| error.to_string())?;
        if binding.owner_task_id != result.task_id {
            return Err("Incomplete: immutable child delegation identity conflicts".into());
        }
        let reminder = crate::session::async_router::background_result_reminder(
            result,
            peri_acp_types::tasks::BgTaskKind::Agent,
        );
        let delivery_id = crate::agent::async_tasks::delivery::terminal_delivery_id(
            &binding.owner_task_id,
            "terminal",
        );
        let command = crate::agent::async_tasks::build_task_terminal_command(
            resources.as_ref(),
            &binding.initiator_session_id,
            binding.recipient_lifecycle,
            &binding,
            delivery_id,
            &reminder,
            peri_acp_types::session::MessageSource::SubAgentComplete,
        )
        .await?;
        let barrier = WorkMutationBarrier::new(resources.clone());
        let mut owned = WorkCommand {
            session_id: admission.session_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: "child-terminal-obligation".into(),
            action: WorkAction::BindTerminalObligation {
                expected_revision: child.head.change_seq,
                admission_id: admission.admission_id.clone(),
                command: Box::new(command.clone()),
            },
        };
        owned.mutation_id = format!(
            "child-terminal-obligation:{}",
            owned.digest().map_err(|error| error.to_string())?
        );
        barrier
            .commit(&owned)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) async fn handoff_terminal(&self) -> Result<(), String> {
        let admission = self
            .turn
            .work_admission()
            .ok_or("Incomplete: child SDK admission unavailable")?;
        let resources = self.resources()?;
        let obligation = crate::session::work_access::terminal(resources.as_ref(), admission)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("Incomplete: durable child terminal obligation unavailable")?;
        let command = obligation.command;
        let barrier = WorkMutationBarrier::new(resources.clone());
        let receipt = barrier
            .commit(&command)
            .await
            .map_err(|error| error.to_string())?;
        let child = resources
            .inspect_work(&WorkQuery::new(
                admission.session_id.clone(),
                WorkSelector::Head,
            ))
            .await
            .map_err(|error| error.to_string())?;
        let mut ack = WorkCommand {
            session_id: admission.session_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: "child-terminal-ack".into(),
            action: WorkAction::AcknowledgeTerminalObligation {
                expected_revision: child.head.change_seq,
                admission_id: admission.admission_id.clone(),
                receipt,
            },
        };
        ack.mutation_id = format!(
            "child-terminal-ack:{}",
            ack.digest().map_err(|error| error.to_string())?
        );
        barrier
            .commit(&ack)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) async fn finish(&self) -> Result<(), String> {
        let Some(admission) = self.turn.work_admission() else {
            return Ok(());
        };
        let port = self
            .port
            .as_ref()
            .ok_or("Incomplete: child SDK settlement capability unavailable")?;
        let resources = self.resources()?;
        let flush = self
            .transcript
            .read()
            .persist_tx_handle()
            .ok_or("Incomplete: child history barrier unavailable")?;
        MessageTranscript::flush_via_tx(&flush)
            .await
            .map_err(|error| error.to_string())?;
        resources
            .drain_persistence(&admission.session_id)
            .await
            .map_err(|error| error.to_string())?;
        let control = resources
            .load_session_control(&admission.session_id)
            .await
            .map_err(|error| error.to_string())?;
        if control.attempt.as_ref() == Some(&admission.execution) {
            let receipt = resources
                .apply_session_control(&ControlCommand {
                    session_id: admission.session_id.clone(),
                    command_id: format!("execution-ended:{}", admission.admission_id),
                    expected_lifecycle: control.lifecycle,
                    expected_revision: control.revision,
                    expected_control_generation: control.control_generation,
                    action: ControlAction::ObserveAttempt { target: None },
                })
                .await
                .map_err(|error| error.to_string())?;
            if receipt.decision != ControlDecision::Accepted {
                return Err("Incomplete: child execution exit remains unconfirmed".into());
            }
        } else if control.attempt.is_some() {
            return Err("Incomplete: another child execution remains observed".into());
        }
        let evidence_id = format!("execution-exit:{}", admission.admission_id);
        let command = WorkCommand {
            session_id: admission.session_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: format!("execution-finish:{}", admission.admission_id),
            action: WorkAction::FinishAdmission {
                admission: admission.clone(),
                evidence_id: evidence_id.clone(),
            },
        };
        WorkMutationBarrier::new(resources)
            .commit(&command)
            .await
            .map_err(|error| error.to_string())?;
        match port
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
            _ => Err("Incomplete: child SDK settlement remains unconfirmed".into()),
        }
    }

    fn resources(&self) -> Result<Arc<dyn SessionResources>, String> {
        self.transcript
            .read()
            .idempotent_reminder_port()
            .map(|(resources, _, _)| resources)
            .ok_or_else(|| "Incomplete: same-store child execution resources unavailable".into())
    }
}
