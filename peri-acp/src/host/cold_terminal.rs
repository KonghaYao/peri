use peri_acp_types::session_resources::work::{
    WorkAction, WorkAdmission, WorkCommand, WorkCommandQuery, WorkDecision, WorkQuery, WorkReceipt,
    WorkRejection, WorkResolution,
};
use peri_acp_types::session_resources::{MutationOutcome, SessionResources};

use crate::transport::types::AcpError;

pub(super) async fn persist(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
    parent_command: WorkCommand,
) -> Result<(), AcpError> {
    for _ in 0..3 {
        let snapshot = resources
            .load_session_work(&WorkQuery {
                session_id: admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(super::workspace::resource_error)?;
        if let Some(existing) = snapshot
            .state
            .terminal_obligations
            .get(&admission.admission_id)
        {
            return if existing == &parent_command {
                Ok(())
            } else {
                Err(incomplete("terminal obligation identity conflict"))
            };
        }
        let mut command = WorkCommand {
            session_id: admission.session_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: "child-terminal-obligation".into(),
            action: WorkAction::BindTerminalObligation {
                expected_revision: snapshot.state.revision,
                admission_id: admission.admission_id.clone(),
                command: Box::new(parent_command.clone()),
            },
        };
        command.mutation_id = format!(
            "child-terminal-obligation:{}",
            command.digest().map_err(super::workspace::resource_error)?
        );
        let receipt = commit(resources, &command).await?;
        match receipt.decision {
            WorkDecision::Accepted => return Ok(()),
            WorkDecision::Rejected {
                reason: WorkRejection::StaleRevision,
            } => continue,
            WorkDecision::Rejected { .. } => {
                return Err(incomplete("terminal obligation was rejected"))
            }
        }
    }
    Err(incomplete("terminal obligation revision did not settle"))
}

pub(super) async fn reconcile(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> Result<bool, AcpError> {
    let query = WorkQuery {
        session_id: admission.session_id.clone(),
        limit: 1,
    };
    let mut child = resources
        .load_session_work(&query)
        .await
        .map_err(super::workspace::resource_error)?;
    for command in &child.pending_commands {
        if matches!(&command.action, WorkAction::BindTerminalObligation { admission_id, .. } if admission_id == &admission.admission_id)
        {
            let receipt = match resources
                .resolve_work_mutation(command)
                .await
                .map_err(super::workspace::resource_error)?
            {
                WorkResolution::Applied { receipt } => receipt,
                WorkResolution::NotApplied => commit(resources, command).await?,
                WorkResolution::Unknown => {
                    return Err(incomplete(
                        "original child terminal obligation remains unknown",
                    ))
                }
            };
            if receipt.decision != WorkDecision::Accepted {
                return Err(incomplete(
                    "original child terminal obligation was rejected",
                ));
            }
        }
    }
    child = resources
        .load_session_work(&query)
        .await
        .map_err(super::workspace::resource_error)?;
    let Some(command) = child
        .state
        .terminal_obligations
        .get(&admission.admission_id)
    else {
        return Ok(false);
    };
    let record = child
        .state
        .admissions
        .get(&admission.admission_id)
        .ok_or_else(|| incomplete("terminal obligation has no admission"))?;
    if record.admission != *admission || child.control.attempt.is_some() {
        return Err(incomplete(
            "terminal obligation has no exact stopped execution proof",
        ));
    }
    let owned = resources
        .load_work_command(&WorkCommandQuery {
            session_id: command.session_id.clone(),
            mutation_id: command.mutation_id.clone(),
        })
        .await
        .map_err(super::workspace::resource_error)?;
    let receipt = match owned {
        Some(owned) if owned.command != *command => {
            return Err(incomplete("parent terminal journal identity conflict"))
        }
        Some(owned) => match owned.resolution {
            Some(WorkResolution::Applied { receipt }) => receipt,
            Some(WorkResolution::NotApplied) => commit(resources, command).await?,
            _ => match resources
                .resolve_work_mutation(command)
                .await
                .map_err(super::workspace::resource_error)?
            {
                WorkResolution::Applied { receipt } => receipt,
                WorkResolution::NotApplied => commit(resources, command).await?,
                WorkResolution::Unknown => {
                    return Err(incomplete(
                        "original parent terminal command remains unknown",
                    ))
                }
            },
        },
        None => commit(resources, command).await?,
    };
    if receipt.session_id != command.session_id
        || receipt.mutation_id != command.mutation_id
        || receipt.decision != WorkDecision::Accepted
    {
        return Err(incomplete("parent terminal ACK unavailable"));
    }
    acknowledge(resources, admission, &receipt).await?;
    Ok(true)
}

async fn acknowledge(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
    parent_receipt: &WorkReceipt,
) -> Result<(), AcpError> {
    for _ in 0..3 {
        let child = resources
            .load_session_work(&WorkQuery {
                session_id: admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(super::workspace::resource_error)?;
        if let Some(existing) = child
            .state
            .terminal_acknowledgements
            .get(&admission.admission_id)
        {
            return if existing == parent_receipt {
                Ok(())
            } else {
                Err(incomplete("parent terminal acknowledgement conflict"))
            };
        }
        let mut command = WorkCommand {
            session_id: admission.session_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: "child-terminal-ack".into(),
            action: WorkAction::AcknowledgeTerminalObligation {
                expected_revision: child.state.revision,
                admission_id: admission.admission_id.clone(),
                receipt: parent_receipt.clone(),
            },
        };
        command.mutation_id = format!(
            "child-terminal-ack:{}",
            command.digest().map_err(super::workspace::resource_error)?
        );
        let receipt = commit(resources, &command).await?;
        match receipt.decision {
            WorkDecision::Accepted => return Ok(()),
            WorkDecision::Rejected {
                reason: WorkRejection::StaleRevision,
            } => continue,
            WorkDecision::Rejected { .. } => {
                return Err(incomplete("parent terminal acknowledgement was rejected"))
            }
        }
    }
    Err(incomplete(
        "parent terminal acknowledgement revision did not settle",
    ))
}

async fn commit(
    resources: &dyn SessionResources,
    command: &WorkCommand,
) -> Result<WorkReceipt, AcpError> {
    let receipt = match resources.apply_work_mutation(command).await {
        Ok(receipt) => receipt,
        Err(error) if error.effect() == MutationOutcome::Unknown => {
            match resources
                .resolve_work_mutation(command)
                .await
                .map_err(super::workspace::resource_error)?
            {
                WorkResolution::Applied { receipt } => receipt,
                _ => return Err(incomplete("original terminal command ACK remains unknown")),
            }
        }
        Err(error) => return Err(super::workspace::resource_error(error)),
    };
    if receipt.session_id != command.session_id || receipt.mutation_id != command.mutation_id {
        return Err(incomplete("terminal command receipt identity conflict"));
    }
    Ok(receipt)
}

fn incomplete(reason: &str) -> AcpError {
    AcpError::new(
        -32010,
        format!("child terminal delegation remains unfinished: {reason}"),
    )
}

#[cfg(test)]
#[path = "cold_terminal_test.rs"]
mod tests;
