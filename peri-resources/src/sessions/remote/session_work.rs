use peri_acp_types::session_resources::work::{
    reduce_work, DeliveryRecord, WorkCommand, WorkDeliveryQuery, WorkQuery, WorkReceipt,
    WorkResolution, WorkSnapshot,
};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceResult};
use turso_serverless::Value;

use super::{
    ledger::{LedgerRow, OperationId, OperationIdentity},
    mutation::{MutationOutcome, OperationResolution, QualifiedMutation},
    session_data::RemoteSessionData,
    sql::{int_at, text_at, StatementSpec},
};
use crate::sessions::{failure::corrupt, work};

fn identity(command: &WorkCommand) -> SessionResourceResult<OperationIdentity> {
    Ok(OperationIdentity::with_digest(
        OperationId::from_record(&format!("session-work.{}", command.mutation_id)),
        "session_work",
        command.digest()?,
    ))
}

impl RemoteSessionData {
    pub(super) async fn read_delivery(
        &self,
        query: &WorkDeliveryQuery,
    ) -> SessionResourceResult<Option<DeliveryRecord>> {
        let row = self
            .store()
            .await?
            .fetch_row(&StatementSpec::new(
                work::READ_DELIVERY,
                vec![
                    Value::Text(query.session_id.clone()),
                    Value::Text(query.delivery_id.clone()),
                ],
            ))
            .await?
            .ok_or_else(|| corrupt("work delivery facts are not readable"))?;
        match row.as_slice() {
            [Value::Integer(0), Value::Null, Value::Null] => Err(super::session_data::not_found()),
            [Value::Integer(1), Value::Null, Value::Null] => work::delivery(query, None, None),
            [Value::Integer(1), Value::Text(kind), Value::Text(json)] => {
                work::delivery(query, Some(kind), Some(json))
            }
            _ => Err(corrupt("work delivery row is not readable")),
        }
    }

    pub(super) async fn read_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkSnapshot> {
        self.read_work_snapshot(query, false)
            .await
            .map(|(snapshot, _)| snapshot)
    }

    async fn read_work_snapshot(
        &self,
        query: &WorkQuery,
        retain_state_json: bool,
    ) -> SessionResourceResult<(WorkSnapshot, Option<String>)> {
        let store = self.store().await?;
        let mut results = store.read_batch(vec![
            StatementSpec::new(crate::sessions::control::READ_STATE,vec![Value::Text(query.session_id.clone())]),
            StatementSpec::new(work::READ_STATE,vec![Value::Text(query.session_id.clone())]),
            StatementSpec::new("SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1),EXISTS(SELECT 1 FROM messages WHERE thread_id=?1)",vec![Value::Text(query.session_id.clone())]),
            StatementSpec::new(work::READ_PENDING, vec![Value::Text(query.session_id.clone())]),
        ]).await?;
        let control = results[0]
            .first()
            .map(|row| text_at(row, 0).ok_or_else(|| corrupt("work control is not readable")))
            .transpose()?;
        let state = results[1]
            .first()
            .map(|row| text_at(row, 0).ok_or_else(|| corrupt("work state is not readable")))
            .transpose()?;
        let facts = results[2]
            .first()
            .ok_or_else(|| corrupt("work session facts are not readable"))?;
        let exists =
            int_at(facts, 0).ok_or_else(|| corrupt("work session facts are not readable"))? != 0;
        let has_history =
            int_at(facts, 1).ok_or_else(|| corrupt("work session facts are not readable"))? != 0;
        if !exists && state.is_none() && control.is_none() {
            return Err(super::session_data::not_found());
        }
        let mut snapshot = WorkSnapshot::from_state(
            query,
            crate::sessions::control::state(control)?,
            work::state(state, has_history)?,
        );
        snapshot.pending_commands = results[3]
            .iter()
            .map(|row| {
                work::original_command(
                    text_at(row, 0).ok_or_else(|| corrupt("owned command is not readable"))?,
                )
            })
            .collect::<SessionResourceResult<_>>()?;
        if !snapshot.pending_commands.is_empty() {
            snapshot.blocked = true;
            snapshot.candidates.clear();
        }
        let state_json = if retain_state_json {
            match results[1].pop().and_then(|row| row.into_iter().next()) {
                Some(Value::Text(json)) => Some(json),
                None => None,
                _ => return Err(corrupt("work state is not readable")),
            }
        } else {
            None
        };
        Ok((snapshot, state_json))
    }

    pub(super) async fn work_resolution(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<Option<WorkResolution>> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                work::READ_RECEIPT,
                vec![Value::Text(command.mutation_id.clone())],
            ))
            .await?;
        row.map(|row| {
            let digest = text_at(&row, 0).ok_or_else(|| corrupt("work digest is not readable"))?;
            let json = text_at(&row, 1).ok_or_else(|| corrupt("work receipt is not readable"))?;
            work::replay(command, digest, json)
        })
        .transpose()
    }

    pub(super) async fn write_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        let identity = identity(command)?;
        if let Some(resolution) = self.work_resolution(command).await? {
            self.acknowledge_work(command).await?;
            return work::receipt(resolution);
        }
        self.begin_owned_work(command).await?;
        for _retry in 0..8 {
            if let Some(resolution) = self.work_resolution(command).await? {
                self.acknowledge_work(command).await?;
                return work::receipt(resolution);
            }
            let (snapshot, state_json) = self
                .read_work_snapshot(
                    &WorkQuery {
                        session_id: command.session_id.clone(),
                        limit: 64,
                    },
                    true,
                )
                .await?;
            let reduction = reduce_work(command, &snapshot.control, &snapshot.state)?;
            let effects = work::mutation_effects(
                command,
                &snapshot.state,
                state_json,
                &snapshot.control,
                &reduction,
            )?
            .into_iter()
            .map(|effect| {
                StatementSpec::new(
                    effect.sql,
                    effect.params.into_iter().map(Value::Text).collect(),
                )
            })
            .collect();
            let store = self.store().await?;
            let outcome = store
                .apply_qualified(&QualifiedMutation {
                    identity: identity.clone(),
                    effects,
                })
                .await?;
            drop(store);
            match outcome {
                MutationOutcome::Applied { .. } => {
                    self.acknowledge_work(command).await?;
                    return self
                        .work_resolution(command)
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
                        .and_then(work::receipt);
                }
                MutationOutcome::Unknown { .. } => {
                    return Err(SessionResourceError::persistence_uncertain(Some(
                        command.session_id.clone(),
                    )))
                }
                MutationOutcome::ClosedNeverApplied => {
                    return Err(SessionResourceError::conflict(
                        "work mutation was finalized without applying",
                    ))
                }
                MutationOutcome::NotApplied {
                    rejected_statement: Some(4),
                    ..
                } => continue,
                other => {
                    if let Some(resolution) = self.work_resolution(command).await? {
                        return work::receipt(resolution);
                    }
                    return Err(other
                        .failure_error(Some(&command.session_id))
                        .unwrap_or_else(|| corrupt("work mutation outcome is not readable")));
                }
            }
        }
        Err(SessionResourceError::persistence_uncertain(Some(
            command.session_id.clone(),
        )))
    }

    pub(super) async fn resolve_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        let identity = identity(command)?;
        if let Some(resolution) = self.work_resolution(command).await? {
            self.acknowledge_work(command).await?;
            return Ok(resolution);
        }
        let store = self.store().await?;
        if let Some(row) = store
            .fetch_row(&StatementSpec::new(
                work::READ_COMMAND,
                vec![Value::Text(command.mutation_id.clone())],
            ))
            .await?
        {
            if work::original_command(
                text_at(&row, 0).ok_or_else(|| corrupt("owned command is not readable"))?,
            )? != *command
            {
                return Err(SessionResourceError::conflict(
                    "work original command identity conflicts",
                ));
            }
        }
        let begin_identity = owned_identity(command, "begin")?;
        if matches!(
            store.close_operation(&begin_identity).await?,
            OperationResolution::StillUnknown { .. }
        ) {
            return Ok(WorkResolution::Unknown);
        }
        let begin_row = store
            .fetch_row(&StatementSpec::new(
                "SELECT digest FROM peri_op_ledger WHERE operation_id=?1",
                vec![Value::Text(begin_identity.operation_id.as_str().to_owned())],
            ))
            .await?;
        if begin_row.as_ref().and_then(|row| text_at(row, 0))
            != Some(begin_identity.digest.as_str())
        {
            return Err(SessionResourceError::conflict(
                "work begin identity conflicts",
            ));
        }
        if let LedgerRow::Applied { digest, .. } =
            store.resolve_operation(&identity.operation_id).await?
        {
            if digest != identity.digest {
                return Err(SessionResourceError::conflict(
                    "work mutation identity conflicts",
                ));
            }
        }
        match store.close_operation(&identity).await? {
            OperationResolution::Applied { .. } => {
                drop(store);
                self.acknowledge_work(command).await?;
                self.work_resolution(command).await?.ok_or_else(|| {
                    SessionResourceError::persistence_uncertain(Some(command.session_id.clone()))
                })
            }
            OperationResolution::ClosedNeverApplied => {
                let row = store
                    .fetch_row(&StatementSpec::new(
                        "SELECT digest FROM peri_op_ledger WHERE operation_id=?1",
                        vec![Value::Text(identity.operation_id.as_str().to_owned())],
                    ))
                    .await?;
                if row.as_ref().and_then(|row| text_at(row, 0)) != Some(identity.digest.as_str()) {
                    return Err(SessionResourceError::conflict(
                        "work mutation identity conflicts",
                    ));
                }
                drop(store);
                self.seal_owned_work(command).await?;
                Ok(WorkResolution::NotApplied)
            }
            OperationResolution::StillUnknown { .. } => Ok(WorkResolution::Unknown),
        }
    }
}

#[path = "session_work_journal.rs"]
mod journal;
use journal::owned_identity;

#[cfg(all(test, not(target_os = "emscripten")))]
#[path = "session_work_test.rs"]
mod tests;
