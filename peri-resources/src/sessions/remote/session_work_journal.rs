use super::*;

pub(super) fn owned_identity(
    command: &WorkCommand,
    phase: &str,
) -> SessionResourceResult<OperationIdentity> {
    Ok(OperationIdentity::with_digest(
        OperationId::from_record(&format!("session-work-{phase}.{}", command.mutation_id)),
        &format!("session_work_{phase}"),
        command.digest()?,
    ))
}

impl RemoteSessionData {
    pub(super) async fn begin_owned_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<()> {
        let store = self.store().await?;
        if let Some(row) = store
            .fetch_row(&StatementSpec::new(
                work::READ_COMMAND,
                vec![Value::Text(command.mutation_id.clone())],
            ))
            .await?
        {
            let original = work::original_command(
                text_at(&row, 0).ok_or_else(|| corrupt("owned command is not readable"))?,
            )?;
            if original != *command {
                return Err(SessionResourceError::conflict(
                    "work original command identity conflicts",
                ));
            }
            return Err(SessionResourceError::persistence_uncertain(Some(
                command.session_id.clone(),
            )));
        }
        let outcome = store
            .apply_qualified(&QualifiedMutation {
                identity: owned_identity(command, "begin")?,
                effects: specifications(work::command_effects(command)?)?,
            })
            .await?;
        match outcome {
            MutationOutcome::Applied {
                replayed: false, ..
            } => Ok(()),
            _ => Err(SessionResourceError::persistence_uncertain(Some(
                command.session_id.clone(),
            ))),
        }
    }

    pub(super) async fn acknowledge_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<()> {
        let store = self.store().await?;
        let outcome = store
            .apply_qualified(&QualifiedMutation {
                identity: owned_identity(command, "ack")?,
                effects: vec![StatementSpec::new(
                    work::ACK_COMMAND,
                    vec![
                        Value::Text(command.mutation_id.clone()),
                        Value::Text(command.digest()?),
                    ],
                )],
            })
            .await?;
        match outcome {
            MutationOutcome::Applied { .. } => Ok(()),
            _ => Err(SessionResourceError::persistence_uncertain(Some(
                command.session_id.clone(),
            ))),
        }
    }

    pub(super) async fn seal_owned_work(&self, command: &WorkCommand) -> SessionResourceResult<()> {
        let mut effects = work::command_effects(command)?;
        effects.push(work_store::SqlStatement::new(
            work::INSERT_RECEIPT,
            vec![
                work_store::SqlParam::Text(command.mutation_id.clone()),
                work_store::SqlParam::Text(command.session_id.clone()),
                work_store::SqlParam::Text(command.digest()?),
                work_store::SqlParam::Text(work::encode(&WorkResolution::NotApplied)?),
            ],
        ));
        effects.push(work_store::SqlStatement::new(
            work::ACK_COMMAND,
            vec![
                work_store::SqlParam::Text(command.mutation_id.clone()),
                work_store::SqlParam::Text(command.digest()?),
            ],
        ));
        let store = self.store().await?;
        let outcome = store
            .apply_qualified(&QualifiedMutation {
                identity: owned_identity(command, "seal")?,
                effects: specifications(effects)?,
            })
            .await?;
        match outcome {
            MutationOutcome::Applied { .. } => Ok(()),
            _ => Err(SessionResourceError::persistence_uncertain(Some(
                command.session_id.clone(),
            ))),
        }
    }
}
