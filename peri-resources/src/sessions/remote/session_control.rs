use peri_acp_types::session_resources::{
    control::decide_control, ControlAction, ControlCommand, ControlReceipt, ControlResolution,
    ControlState, SessionResourceError, SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use turso_serverless::Value;

use super::{
    ledger::{LedgerRow, OperationId, OperationIdentity},
    mutation::{MutationOutcome, OperationResolution, QualifiedMutation},
    session_data::RemoteSessionData,
    sql::{text_at, StatementSpec},
};
use crate::sessions::{control, failure::corrupt};

fn identity(command: &ControlCommand) -> SessionResourceResult<OperationIdentity> {
    Ok(OperationIdentity::with_digest(
        OperationId::from_record(&format!("session-control.{}", command.command_id)),
        "session_control",
        command.digest()?,
    ))
}

#[cfg(all(test, not(target_os = "emscripten")))]
#[path = "session_control_test.rs"]
mod tests;

impl RemoteSessionData {
    pub(super) async fn read_control(&self, id: &ThreadId) -> SessionResourceResult<ControlState> {
        let store = self.store().await?;
        let (facts, rows) = store
            .read_pair(
                StatementSpec::new(
                    "SELECT 1 FROM threads WHERE id = ?1",
                    vec![Value::Text(id.clone())],
                ),
                StatementSpec::new(control::READ_STATE, vec![Value::Text(id.clone())]),
            )
            .await?;
        if facts.is_empty() && rows.is_empty() {
            return Err(super::session_data::not_found());
        }
        let json = rows
            .first()
            .map(|row| text_at(row, 0).ok_or_else(|| corrupt("control state is not readable")))
            .transpose()?;
        control::state(json)
    }

    async fn control_resolution(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<Option<ControlResolution>> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                control::READ_RECEIPT,
                vec![Value::Text(command.command_id.clone())],
            ))
            .await?;
        row.map(|row| {
            let digest =
                text_at(&row, 0).ok_or_else(|| corrupt("control digest is not readable"))?;
            let json =
                text_at(&row, 1).ok_or_else(|| corrupt("control receipt is not readable"))?;
            control::replay(command, digest, json)
        })
        .transpose()
    }

    pub(super) async fn write_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlReceipt> {
        let identity = identity(command)?;
        for _retry in 0..8 {
            if let Some(resolution) = self.control_resolution(command).await? {
                return control::receipt(resolution);
            }
            let current = self.read_control(&command.session_id).await?;
            if command.action != ControlAction::FinishClose
                && !self.exists(&command.session_id).await?
            {
                return Err(super::session_data::not_found());
            }
            let receipt = decide_control(command, &current);
            let current_json = control::encode(&current)?;
            let resolution = ControlResolution::Applied {
                receipt: receipt.clone(),
            };
            let mut effects = vec![
                StatementSpec::new(
                    control::INSERT_STATE,
                    vec![
                        Value::Text(command.session_id.clone()),
                        Value::Text(current_json.clone()),
                    ],
                ),
                StatementSpec::new(
                    control::GUARD_STATE,
                    vec![
                        Value::Text(command.session_id.clone()),
                        Value::Text(current_json),
                        Value::Integer(i64::from(command.action == ControlAction::FinishClose)),
                    ],
                ),
                StatementSpec::new(
                    control::UPDATE_STATE,
                    vec![
                        Value::Text(command.session_id.clone()),
                        Value::Text(control::encode(&receipt.state)?),
                    ],
                ),
                StatementSpec::new(
                    control::INSERT_RECEIPT,
                    vec![
                        Value::Text(command.command_id.clone()),
                        Value::Text(command.session_id.clone()),
                        Value::Text(identity.digest.clone()),
                        Value::Text(control::encode(&resolution)?),
                    ],
                ),
            ];
            match control::closing_projection(command, &receipt) {
                Some(true) => effects.push(StatementSpec::new(
                    control::MARK_CLOSING,
                    vec![
                        Value::Text(command.session_id.clone()),
                        Value::Text(peri_time::now_utc_rfc3339()),
                    ],
                )),
                Some(false) => effects.push(StatementSpec::new(
                    control::CLEAR_CLOSING,
                    vec![Value::Text(command.session_id.clone())],
                )),
                None => {}
            }
            let store = self.store().await?;
            let outcome = store
                .apply_qualified(&QualifiedMutation {
                    identity: identity.clone(),
                    effects,
                })
                .await?;
            match outcome {
                MutationOutcome::Applied { .. } => {
                    drop(store);
                    return self
                        .control_resolution(command)
                        .await
                        .map_err(|_| {
                            SessionResourceError::persistence_uncertain(Some(
                                command.session_id.clone(),
                            ))
                        })?
                        .ok_or_else(|| {
                            SessionResourceError::persistence_uncertain(Some(
                                command.session_id.clone(),
                            ))
                        })
                        .and_then(control::receipt);
                }
                MutationOutcome::Unknown { .. } => {
                    return Err(SessionResourceError::persistence_uncertain(Some(
                        command.session_id.clone(),
                    )))
                }
                MutationOutcome::ClosedNeverApplied => {
                    return Err(SessionResourceError::conflict(
                        "control command was finalized without applying",
                    ))
                }
                MutationOutcome::NotApplied {
                    rejected_statement: Some(2),
                    ..
                } => continue,
                other => {
                    drop(store);
                    if let Some(resolution) = self.control_resolution(command).await? {
                        return control::receipt(resolution);
                    }
                    return Err(other
                        .failure_error(Some(&command.session_id))
                        .unwrap_or_else(|| corrupt("control mutation outcome is not readable")));
                }
            }
        }
        Err(SessionResourceError::conflict(
            "control state changed during mutation; retry original command",
        ))
    }

    pub(super) async fn resolve_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlResolution> {
        let identity = identity(command)?;
        if let Some(resolution) = self.control_resolution(command).await? {
            return Ok(resolution);
        }
        let store = self.store().await?;
        if let LedgerRow::Applied { digest, .. } =
            store.resolve_operation(&identity.operation_id).await?
        {
            if digest != identity.digest {
                return Err(SessionResourceError::conflict(
                    "control command identity conflicts",
                ));
            }
        }
        match store.close_operation(&identity).await? {
            OperationResolution::Applied { .. } => {
                drop(store);
                self.control_resolution(command)
                    .await?
                    .ok_or_else(|| corrupt("applied control receipt is missing"))
            }
            OperationResolution::ClosedNeverApplied => {
                let row = store
                    .fetch_row(&StatementSpec::new(
                        "SELECT digest FROM peri_op_ledger WHERE operation_id = ?1",
                        vec![Value::Text(identity.operation_id.as_str().to_owned())],
                    ))
                    .await?;
                if row.as_ref().and_then(|row| text_at(row, 0)) != Some(identity.digest.as_str()) {
                    return Err(SessionResourceError::conflict(
                        "control command identity conflicts",
                    ));
                }
                Ok(ControlResolution::NotApplied)
            }
            OperationResolution::StillUnknown { .. } => Ok(ControlResolution::Unknown),
        }
    }
}
