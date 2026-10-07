use peri_acp_types::session_resources::work::{
    reduce_work, DeliveryRecord, PreparedWorkCommand, WorkCommand, WorkDeliveryQuery, WorkQuery,
    WorkReceipt, WorkResolution, WorkSnapshot, WorkState,
};
use peri_acp_types::session_resources::{
    ControlState, SessionResourceError, SessionResourceResult,
};
use turso_serverless::Value;

use super::{
    ledger::{LedgerRow, OperationId, OperationIdentity},
    mutation::{MutationOutcome, OperationResolution, QualifiedMutation},
    session_data::RemoteSessionData,
    sql::{int_at, text_at, StatementSpec},
};
use crate::sessions::{failure::corrupt, work};

fn identity(command: &PreparedWorkCommand, digest: &str) -> OperationIdentity {
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
                work::resource_owner_facts(work::ResourceOwnerRow {
                    session_exists: *exists != 0,
                    control_json: text_at(&row, 1),
                    state_exists: *state_exists != 0,
                    revision_json: text_at(&row, 3),
                    current_owner_json: text_at(&row, 4),
                    previous_owner_json: text_at(&row, 5),
                    current_child_json: text_at(&row, 6),
                    previous_child_json: text_at(&row, 7),
                    owners_type: text_at(&row, 8),
                    child_metadata_type: text_at(&row, 9),
                })
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
            [Value::Integer(session_exists), Value::Integer(control_exists), Value::Integer(state_exists), Value::Null | Value::Text(_)] => {
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
        let (control, state, _, pending) = self.read_work_state(&query.session_id, true).await?;
        let mut snapshot = WorkSnapshot::from_state(query, control, state);
        snapshot.pending_commands = pending;
        if !snapshot.pending_commands.is_empty() {
            snapshot.blocked = true;
            snapshot.candidates.clear();
        }
        Ok(snapshot)
    }

    async fn read_work_state(
        &self,
        session_id: &str,
        include_pending: bool,
    ) -> SessionResourceResult<(ControlState, WorkState, Option<String>, Vec<WorkCommand>)> {
        let mut phase = work::WorkPhase::new("remote", "snapshot_query_decode");
        let store = self.store().await?;
        let mut statements = vec![StatementSpec::new(
            work::READ_SNAPSHOT,
            vec![Value::Text(session_id.to_owned())],
        )];
        if include_pending {
            statements.push(StatementSpec::new(
                work::READ_PENDING,
                vec![Value::Text(session_id.to_owned())],
            ));
        }
        phase.query_count = statements.len();
        let mut results = store.read_batch(statements).await?;
        let facts = results
            .first()
            .and_then(|rows| rows.first())
            .ok_or_else(|| corrupt("work session facts are not readable"))?;
        if !matches!(
            facts.as_slice(),
            [
                Value::Integer(_),
                Value::Null | Value::Text(_),
                Value::Null | Value::Text(_),
                Value::Integer(_)
            ]
        ) {
            return Err(corrupt("work session facts are not readable"));
        }
        let exists =
            int_at(facts, 0).ok_or_else(|| corrupt("work session facts are not readable"))? != 0;
        let has_history =
            int_at(facts, 3).ok_or_else(|| corrupt("work session facts are not readable"))? != 0;
        let control = text_at(facts, 1);
        let state = text_at(facts, 2);
        if !exists && state.is_none() && control.is_none() {
            return Err(super::session_data::not_found());
        }
        phase.state_bytes = state.map_or(0, str::len);
        let control = crate::sessions::control::state(control)?;
        let state = work::state(state, has_history)?;
        let pending = if include_pending {
            results
                .get(1)
                .ok_or_else(|| corrupt("work pending commands are not readable"))?
                .iter()
                .map(|row| {
                    work::original_command(
                        text_at(row, 0).ok_or_else(|| corrupt("owned command is not readable"))?,
                    )
                })
                .collect::<SessionResourceResult<_>>()?
        } else {
            Vec::new()
        };
        let state_json = match results[0].pop().and_then(|row| row.into_iter().nth(2)) {
            Some(Value::Text(json)) => Some(json),
            Some(Value::Null) => None,
            _ => return Err(corrupt("work state is not readable")),
        };
        Ok((control, state, state_json, pending))
    }

    pub(super) async fn work_resolution(
        &self,
        command: &PreparedWorkCommand,
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
            work::replay_with_digest(command.command(), command.digest(), digest, json)
        })
        .transpose()
    }

    pub(super) async fn write_work(
        &self,
        command: &PreparedWorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        let digest = command.digest();
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
            let (control, state, state_json, _) =
                self.read_work_state(&command.session_id, false).await?;
            let initial_json = work::pre_state_json(state_json, &state)?;
            let mut phase = work::WorkPhase::new("remote", "reduce_encode");
            phase.state_bytes = initial_json.len();
            let parent_command = work::terminal_parent_command(command, &state)?;
            let reduction = reduce_work(command, &control, state)?;
            let effects = work::mutation_effects(
                command,
                initial_json,
                parent_command.as_ref(),
                &control,
                &reduction,
            )?;
            phase.command_bytes = command.encoded().len();
            phase.record_effects(&effects);
            drop(reduction);
            drop(parent_command);
            drop(control);
            let effects = effects
                .into_iter()
                .map(|effect| {
                    StatementSpec::new(
                        effect.sql,
                        effect
                            .params
                            .into_iter()
                            .map(|value| Value::Text(value.to_string()))
                            .collect(),
                    )
                })
                .collect::<Vec<_>>();
            drop(phase);
            let store = self.store().await?;
            let mut phase = work::WorkPhase::new("remote", "qualified_mutation");
            phase.query_count = effects.len();
            phase.transaction_count = 1;
            let outcome = store
                .apply_qualified(&QualifiedMutation {
                    identity: identity.clone(),
                    effects,
                })
                .await?;
            drop(phase);
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
        command: &PreparedWorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        let identity = identity(command, command.digest());
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
            )? != *command.command()
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
