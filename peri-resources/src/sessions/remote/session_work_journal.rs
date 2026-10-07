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

fn specifications(effects: Vec<work::WorkEffect>) -> Vec<StatementSpec> {
    effects
        .into_iter()
        .map(|effect| {
            StatementSpec::new(
                effect.sql,
                effect.params.into_iter().map(Value::Text).collect(),
            )
        })
        .collect()
}

impl RemoteSessionData {
    pub(in crate::sessions::remote) async fn read_work_command(
        &self,
        query: &peri_acp_types::session_resources::work::WorkCommandQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::OwnedWorkCommand>>
    {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                work::READ_OWNED_COMMAND,
                vec![
                    Value::Text(query.mutation_id.clone()),
                    Value::Text(query.session_id.clone()),
                ],
            ))
            .await?;
        row.map(|row| {
            work::owned_command(
                text_at(&row, 0).ok_or_else(|| corrupt("owned command is not readable"))?,
                text_at(&row, 1).ok_or_else(|| corrupt("owned command digest is not readable"))?,
                text_at(&row, 2),
                int_at(&row, 3)
                    .ok_or_else(|| corrupt("owned command reconciliation is not readable"))?
                    != 0,
            )
        })
        .transpose()
    }
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
                effects: specifications(work::command_effects(command)?),
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
        effects.push(work::WorkEffect::texts(
            work::INSERT_RECEIPT,
            [
                command.mutation_id.clone(),
                command.session_id.clone(),
                command.digest()?,
                work::encode(&WorkResolution::NotApplied)?,
            ],
        ));
        effects.push(work::WorkEffect::texts(
            work::ACK_COMMAND,
            [command.mutation_id.clone(), command.digest()?],
        ));
        let store = self.store().await?;
        let outcome = store
            .apply_qualified(&QualifiedMutation {
                identity: owned_identity(command, "seal")?,
                effects: specifications(effects),
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
