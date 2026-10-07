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

fn identity(command: &WorkCommand, digest: &str) -> OperationIdentity {
    OperationIdentity::with_digest(
        OperationId::from_record(&format!("session-work.{}", command.mutation_id)),
        "session_work",
        digest.to_owned(),
    )
}

impl RemoteSessionData {
    pub(super) async fn read_resource_owner_facts(
        &self,
        id: &peri_acp_types::thread::ThreadId,
        previous_lifecycle: u64,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::ResourceOwnerFacts> {
        let row = self
            .store()
            .await?
            .fetch_row(&StatementSpec::new(
                work::READ_RESOURCE_OWNER_FACTS,
                vec![
                    Value::Text(id.clone()),
                    Value::Text(previous_lifecycle.to_string()),
                ],
            ))
            .await?
            .ok_or_else(|| corrupt("resource owner facts are not readable"))?;
        match row.as_slice() {
            [Value::Integer(exists), control, Value::Integer(state_exists), revision, current_owner, previous_owner, current_child, previous_child, owners_type, child_type]
                if [
                    control,
                    revision,
                    current_owner,
                    previous_owner,
                    current_child,
                    previous_child,
                    owners_type,
                    child_type,
                ]
                .iter()
                .all(|value| matches!(value, Value::Null | Value::Text(_))) =>
            {
                work::resource_owner_facts(
                    *exists != 0,
                    text_at(&row, 1),
                    *state_exists != 0,
                    text_at(&row, 3),
                    text_at(&row, 4),
                    text_at(&row, 5),
                    text_at(&row, 6),
                    text_at(&row, 7),
                    text_at(&row, 8),
                    text_at(&row, 9),
                )
            }
            _ => Err(corrupt("resource owner facts are not readable")),
        }
    }
    pub(super) async fn read_work_revision(
        &self,
        id: &peri_acp_types::thread::ThreadId,
    ) -> SessionResourceResult<u64> {
        let row = self
            .store()
            .await?
            .fetch_row(&StatementSpec::new(
                work::READ_REVISION,
                vec![Value::Text(id.clone())],
            ))
            .await?
            .ok_or_else(|| corrupt("work revision row is not readable"))?;
        match row.as_slice() {
            [Value::Integer(session_exists), Value::Integer(control_exists), Value::Integer(state_exists), revision_json]
                if matches!(revision_json, Value::Null | Value::Text(_)) =>
            {
                work::revision(
                    *session_exists,
                    *control_exists,
                    *state_exists,
                    text_at(&row, 3),
                )
            }
            _ => Err(corrupt("work revision row is not readable")),
        }
    }

    pub(super) async fn read_work_availability(
        &self,
        id: &peri_acp_types::thread::ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkAvailability> {
        let results = self
            .store()
            .await?
            .read_batch(vec![StatementSpec::new(
                work::READ_AVAILABILITY,
                vec![Value::Text(id.clone())],
            )])
            .await?;
        let row = results
            .first()
            .and_then(|rows| rows.first())
            .ok_or_else(|| corrupt("work availability facts are not readable"))?;
        match row.as_slice() {
            [Value::Integer(exists), control, facts, Value::Integer(history)]
                if matches!(control, Value::Null | Value::Text(_))
                    && matches!(facts, Value::Null | Value::Text(_)) =>
            {
                work::availability(
                    *exists != 0,
                    text_at(row, 1),
                    text_at(row, 2),
                    *history != 0,
                )
            }
            _ => Err(corrupt("work availability row is not readable")),
        }
    }

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
        let digest = command.digest()?;
        let identity = identity(command, &digest);
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
            let initial_json = work::pre_state_json(state_json, &snapshot.state)?;
            let parent_command = work::terminal_parent_command(command, &snapshot.state);
            let reduction = reduce_work(command, &snapshot.control, snapshot.state)?;
            let effects = work::mutation_effects(
                command,
                &digest,
                initial_json,
                parent_command.as_ref(),
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
        let identity = identity(command, &command.digest()?);
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
