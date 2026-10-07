use peri_acp_types::session_resources::{
    work::{
        WorkAction, WorkAdmission, WorkCommand, WorkCommandQuery, WorkDecision, WorkResolution,
    },
    SessionResources,
};

use crate::transport::types::AcpError;

fn command(admission: &WorkAdmission, retry: usize) -> WorkCommand {
    let mutation_id = if retry == 0 {
        format!("execution-finish:{}", admission.admission_id)
    } else {
        format!("execution-finish:{}:retry:{retry}", admission.admission_id)
    };
    WorkCommand {
        session_id: admission.session_id.clone(),
        recipient_lifecycle: admission.lifecycle,
        mutation_id,
        action: WorkAction::FinishAdmission {
            admission: admission.clone(),
            evidence_id: format!("execution-exit:{}", admission.admission_id),
        },
    }
}

pub(super) async fn reconcile_finish(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> Result<bool, AcpError> {
    let original = command(admission, 0);
    let owned = resources
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .map_err(super::super::workspace::resource_error)?;
    match owned {
        None => Ok(false),
        Some(owned) if owned.command != original => Err(AcpError::new(
            -32010,
            "execution finish journal identity conflict",
        )),
        Some(_) => {
            finish_admission(resources, admission).await?;
            Ok(true)
        }
    }
}

pub(super) async fn finish_admission(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> Result<String, AcpError> {
    let mut retry = 0;
    let mut new_attempts = 0;
    loop {
        let original = command(admission, retry);
        let owned = resources
            .load_work_command(&WorkCommandQuery {
                session_id: original.session_id.clone(),
                mutation_id: original.mutation_id.clone(),
            })
            .await
            .map_err(super::super::workspace::resource_error)?;
        let resolution = match owned {
            Some(owned) => {
                if owned.command != original {
                    return Err(AcpError::new(
                        -32010,
                        "execution finish journal identity conflict",
                    ));
                }
                resources
                    .resolve_work_mutation(&original)
                    .await
                    .map_err(super::super::workspace::resource_error)?
            }
            None => {
                if new_attempts == 3 {
                    return Err(AcpError::new(
                        -32010,
                        "execution settlement remains unconfirmed",
                    ));
                }
                new_attempts += 1;
                match resources.apply_work_mutation(&original).await {
                    Ok(receipt) => WorkResolution::Applied { receipt },
                    Err(_) => resources
                        .resolve_work_mutation(&original)
                        .await
                        .map_err(super::super::workspace::resource_error)?,
                }
            }
        };
        match resolution {
            WorkResolution::Applied { receipt } if receipt.decision == WorkDecision::Accepted => {
                return Ok(format!("execution-exit:{}", admission.admission_id));
            }
            WorkResolution::Applied { .. } => {
                return Err(AcpError::new(-32010, "execution settlement was rejected"));
            }
            WorkResolution::Unknown => {
                return Err(AcpError::new(
                    -32010,
                    "execution settlement remains unknown",
                ));
            }
            WorkResolution::NotApplied => {
                retry = retry.checked_add(1).ok_or_else(|| {
                    AcpError::new(-32010, "execution settlement retry identity exhausted")
                })?;
            }
        }
    }
}

#[cfg(test)]
#[path = "execution_finish_test.rs"]
mod tests;
