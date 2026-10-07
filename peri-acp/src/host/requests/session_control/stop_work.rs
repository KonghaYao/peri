use peri_acp_types::session_resources::{
    work::{
        WorkAction, WorkCommand, WorkCommandQuery, WorkDecision, WorkQuery, WorkReceipt,
        WorkResolution, WorkSnapshot, WorkStage, WorkTarget,
    },
    ControlAction, ControlCommand, ControlDecision, ControlReceipt, ControlStatus,
    SessionResources,
};
use sha2::{Digest, Sha256};

use crate::host::workspace::resource_error;
use crate::transport::types::AcpError;

pub(super) async fn abandon_owned_work(
    resources: &dyn SessionResources,
    command: &ControlCommand,
    original: &ControlReceipt,
) -> Result<Vec<WorkReceipt>, AcpError> {
    let ControlAction::Stop { target } = &command.action else {
        return Ok(Vec::new());
    };
    if original.decision != ControlDecision::Accepted
        || original.command_id != command.command_id
        || original.session_id != command.session_id
        || original.state.lifecycle != command.expected_lifecycle
        || original.state.status != ControlStatus::Paused
    {
        return Err(AcpError::new(
            -32010,
            "Stop work settlement requires the original accepted receipt",
        ));
    }
    let query = WorkQuery {
        session_id: command.session_id.clone(),
        limit: 1,
    };
    let pending = resources
        .load_session_work(&query)
        .await
        .map_err(resource_error)?;
    if !same_stop_generation(&pending, original) {
        return Ok(Vec::new());
    }
    let mut receipts = Vec::new();
    for frozen in &pending.pending_commands {
        let resolution = resources
            .resolve_work_mutation(frozen)
            .await
            .map_err(resource_error)?;
        match resolution {
            WorkResolution::Unknown => return Err(unconfirmed(&frozen.mutation_id)),
            WorkResolution::NotApplied => {
                if matches!(&frozen.action, WorkAction::AbandonWork { authorization_ref, .. } if authorization_ref == &command.command_id)
                {
                    return Err(AcpError::new(-32010, "Original Stop work settlement was not applied; a new explicit control command is required")
                        .with_data(serde_json::json!({"status":"notApplied","mutationId":frozen.mutation_id})));
                }
            }
            WorkResolution::Applied { receipt } => {
                if matches!(&frozen.action, WorkAction::AbandonWork { authorization_ref, .. } if authorization_ref == &command.command_id)
                {
                    accept(&receipt)?;
                    receipts.push(receipt);
                }
            }
        }
    }
    loop {
        let snapshot = resources
            .load_session_work(&query)
            .await
            .map_err(resource_error)?;
        if !same_stop_generation(&snapshot, original) {
            return Ok(receipts);
        }
        if !snapshot.pending_commands.is_empty() {
            return Err(unconfirmed(&snapshot.pending_commands[0].mutation_id));
        }
        let owned = snapshot.state.works.values().find(|work| {
            !matches!(work.stage, WorkStage::Abandoned | WorkStage::Settled)
                && snapshot
                    .state
                    .batches
                    .get(&work.batch_id)
                    .is_some_and(|batch| {
                        batch.recipient_lifecycle == original.state.lifecycle
                            && batch.execution == *target
                    })
        });
        let Some(work) = owned else {
            return Ok(receipts);
        };
        let identity = mutation_id(command, &work.work_id);
        if let Some(owned_command) = resources
            .load_work_command(&WorkCommandQuery {
                session_id: command.session_id.clone(),
                mutation_id: identity.clone(),
            })
            .await
            .map_err(resource_error)?
        {
            let resolution = match owned_command.resolution {
                Some(resolution) if !owned_command.pending => resolution,
                _ => resources
                    .resolve_work_mutation(&owned_command.command)
                    .await
                    .map_err(resource_error)?,
            };
            match resolution {
                WorkResolution::Applied { receipt } => {
                    accept(&receipt)?;
                    receipts.push(receipt);
                    continue;
                }
                WorkResolution::NotApplied => return Err(AcpError::new(-32010,
                    "Original Stop work settlement was not applied; a new explicit control command is required")
                    .with_data(serde_json::json!({"status":"notApplied","mutationId":identity}))),
                WorkResolution::Unknown => return Err(unconfirmed(&identity)),
            }
        }
        let mutation = WorkCommand {
            session_id: command.session_id.clone(),
            recipient_lifecycle: original.state.lifecycle,
            mutation_id: identity,
            action: WorkAction::AbandonWork {
                expected_revision: snapshot.state.revision,
                expected_control_generation: original.state.control_generation,
                target: WorkTarget { work_id: work.work_id.clone(), expected_work_revision: work.revision },
                reason: "processing abandoned by explicit Stop; external resource outcomes remain independently recorded".into(),
                authorization_ref: command.command_id.clone(),
            },
        };
        let receipt = match resources.apply_work_mutation(&mutation).await {
            Ok(receipt) => receipt,
            Err(_) => match resources
                .resolve_work_mutation(&mutation)
                .await
                .map_err(resource_error)?
            {
                WorkResolution::Applied { receipt } => receipt,
                WorkResolution::NotApplied => return Err(AcpError::new(
                    -32010,
                    "Stop work settlement was not applied",
                )
                .with_data(
                    serde_json::json!({"status":"notApplied","mutationId":mutation.mutation_id}),
                )),
                WorkResolution::Unknown => return Err(unconfirmed(&mutation.mutation_id)),
            },
        };
        accept(&receipt)?;
        receipts.push(receipt);
    }
}

fn same_stop_generation(snapshot: &WorkSnapshot, original: &ControlReceipt) -> bool {
    snapshot.control.lifecycle == original.state.lifecycle
        && snapshot.control.control_generation == original.state.control_generation
        && snapshot.control.status == ControlStatus::Paused
}

fn accept(receipt: &WorkReceipt) -> Result<(), AcpError> {
    if let WorkDecision::Rejected { reason } = &receipt.decision {
        return Err(AcpError::new(
            -32010,
            format!("Stop work settlement was rejected: {reason:?}"),
        )
        .with_data(serde_json::json!({"status":"rejected","receipt":receipt})));
    }
    Ok(())
}

fn unconfirmed(mutation_id: &str) -> AcpError {
    AcpError::new(
        -32010,
        "Stop work settlement remains unknown; resolve the original command",
    )
    .with_data(serde_json::json!({"status":"unknown","mutationId":mutation_id}))
}

fn mutation_id(command: &ControlCommand, work_id: &str) -> String {
    format!(
        "stop-abandon:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&command.session_id, &command.command_id, work_id))
                .expect("Stop identity serializes")
        )
    )
}

#[cfg(test)]
#[path = "stop_work_test.rs"]
mod tests;
