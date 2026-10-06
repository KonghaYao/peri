use peri_acp_types::session_resources::work::{
    WorkAction, WorkAdmission, WorkCommand, WorkDecision, WorkQuery, WorkReceipt, WorkRejection,
    WorkResolution, WorkSelector,
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
            .inspect_work(&WorkQuery {
                session_id: admission.session_id.clone(),
                selector: WorkSelector::TerminalCommand {
                    admission_id: admission.admission_id.clone(),
                },
                limit: 1,
                cursor: None,
            })
            .await
            .map_err(super::workspace::resource_error)?;
        if let Some(existing) = super::work_query::terminal(&snapshot, &admission.admission_id)? {
            return if existing.command.as_ref() == &parent_command {
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
                expected_revision: snapshot.head.change_seq,
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
                return Err(incomplete("terminal obligation was rejected"));
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
        selector: WorkSelector::TerminalCommand {
            admission_id: admission.admission_id.clone(),
        },
        limit: 1,
        cursor: None,
    };
    let child = super::work_recovery::resolve_pending(resources, &query)
        .await
        .map_err(super::workspace::resource_error)?;
    let Some(obligation) = super::work_query::terminal(&child, &admission.admission_id)? else {
        return Ok(false);
    };
    let command = obligation.command.as_ref();
    let admitted = super::work_query::inspect(
        resources,
        &admission.session_id,
        WorkSelector::Admission {
            admission_id: admission.admission_id.clone(),
        },
    )
    .await?;
    let record = super::work_query::admission(&admitted, &admission.admission_id)?
        .ok_or_else(|| incomplete("terminal obligation has no admission"))?;
    if record.admission != *admission || child.control.attempt.is_some() {
        return Err(incomplete(
            "terminal obligation has no exact stopped execution proof",
        ));
    }
    let owned =
        super::work_query::command(resources, &command.session_id, &command.mutation_id).await?;
    let receipt = match owned {
        Some(owned) if owned.command != *command => {
            return Err(incomplete("parent terminal journal identity conflict"));
        }
        Some(owned) => match owned.resolution {
            Some(WorkResolution::Applied { receipt }) => receipt,
            Some(WorkResolution::NotApplied) => return Err(incomplete("original parent terminal command was sealed not applied")),
            _ => match resources
                .resolve_work_mutation(command)
                .await
                .map_err(super::workspace::resource_error)?
            {
                WorkResolution::Applied { receipt } => receipt,
                WorkResolution::NotApplied => return Err(incomplete("original parent terminal command was sealed not applied")),
                WorkResolution::Unknown => {
                    return Err(incomplete(
                        "original parent terminal command remains unknown",
                    ));
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
            .inspect_work(&WorkQuery {
                session_id: admission.session_id.clone(),
                selector: WorkSelector::TerminalCommand {
                    admission_id: admission.admission_id.clone(),
                },
                limit: 1,
                cursor: None,
            })
            .await
            .map_err(super::workspace::resource_error)?;
        if let Some(existing) = super::work_query::terminal(&child, &admission.admission_id)?
            .and_then(|obligation| obligation.acknowledgement.as_ref())
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
                expected_revision: child.head.change_seq,
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
                return Err(incomplete("parent terminal acknowledgement was rejected"));
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
