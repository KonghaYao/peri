use peri_acp_types::session_resources::work::{
    transition_work, EvidenceQuery, EvidenceRecord, EvidenceWrite, PayloadRef, WorkCommand,
    WorkInspection, WorkQuery, WorkReceipt, WorkResolution,
};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceResult};
use turso_serverless::Value;

use super::{
    ledger::{LedgerRow, OperationId, OperationIdentity},
    mutation::{MutationOutcome, OperationResolution, QualifiedMutation},
    session_data::RemoteSessionData,
    sql::{text_at, StatementSpec},
};
use crate::sessions::{failure::corrupt, work, work_store};

#[path = "work_records/transport.rs"]
mod transport;
pub(super) use transport::{rows, specifications, validate_budget};

fn identity(command: &WorkCommand) -> SessionResourceResult<OperationIdentity> {
    Ok(OperationIdentity::with_digest(
        OperationId::from_record(&format!("session-work.{}", command.mutation_id)),
        "session_work",
        command.digest()?,
    ))
}

impl RemoteSessionData {
    pub(super) async fn read_work(
        &self,
        query: &WorkQuery,
    ) -> SessionResourceResult<WorkInspection> {
        let statements = specifications(work_store::inspection_plan(query)?)?;
        let results = rows(self.store().await?.read_batch(statements).await?)?;
        work_store::decode_inspection(query, &results)
    }

    pub(super) async fn read_work_evidence(
        &self,
        query: &EvidenceQuery,
    ) -> SessionResourceResult<EvidenceRecord> {
        let statements = specifications(work_store::evidence_plan(query)?)?;
        let results = rows(self.store().await?.read_batch(statements).await?)?;
        work_store::decode_evidence(query, &results)
    }

    pub(super) async fn prepare_work_evidence(
        &self,
        write: &EvidenceWrite,
    ) -> SessionResourceResult<PayloadRef> {
        let (reference, statements) = work_store::prepare_evidence_plan(write)?;
        let effects = specifications(statements)?;
        self.commit_effects(
            "prepare_evidence",
            &[write.session_id.clone(), work::encode(&reference)?],
            effects,
            &write.session_id,
        )
        .await?;
        let prepared = async {
            let rows = rows(
                self.store()
                    .await?
                    .read_batch(specifications(vec![
                        work_store::payload::prepared_evidence_plan(write)?,
                    ])?)
                    .await?,
            )?;
            work_store::payload::prepared_evidence_reference(write, &rows[0])
        }
        .await;
        prepared.map_err(|error| {
            tracing::error!(%error, session_id = %write.session_id, "remote immutable evidence readback was not confirmed after commit");
            SessionResourceError::persistence_uncertain(Some(write.session_id.clone()))
        })
    }

    pub(super) async fn work_resolution(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<Option<WorkResolution>> {
        let row = self
            .store()
            .await?
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
        let read_plan = specifications(work_store::read_set(command)?)?;
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
            let store = self.store().await?;
            let results = rows(store.read_batch(read_plan.clone()).await?)?;
            let facts = work_store::decode_facts(command, &results)?;
            let mut transition = transition_work(command, &facts)?;
            if let Some(statement) = work_store::response_validation_plan(command, &transition)? {
                let validation = rows(store.read_batch(specifications(vec![statement])?).await?)?;
                work_store::validate_response_transition(command, &mut transition, &validation[0])?;
            }
            let mut effects = specifications(work_store::sql_plan(command, &facts, &transition)?)?;
            let retry_guards = effects
                .iter()
                .enumerate()
                .filter_map(|(index, statement)| {
                    statement
                        .sql
                        .contains("SELECT NULL WHERE")
                        .then_some(index + 1)
                })
                .collect::<Vec<_>>();
            effects.push(StatementSpec::new(
                work::INSERT_RECEIPT,
                vec![
                    Value::Text(command.mutation_id.clone()),
                    Value::Text(command.session_id.clone()),
                    Value::Text(command.digest()?),
                    Value::Text(work::encode(&WorkResolution::Applied {
                        receipt: transition.receipt,
                    })?),
                ],
            ));
            effects.push(StatementSpec::new(
                work::ACK_COMMAND,
                vec![
                    Value::Text(command.mutation_id.clone()),
                    Value::Text(command.digest()?),
                ],
            ));
            validate_budget(&effects)?;
            let outcome = store
                .apply_qualified(&QualifiedMutation {
                    identity: identity.clone(),
                    effects,
                })
                .await?;
            drop(store);
            match outcome {
                MutationOutcome::Applied { .. } => {
                    return self
                        .work_resolution(command)
                        .await
                        .map_err(|_| uncertain(command))?
                        .ok_or_else(|| uncertain(command))
                        .and_then(work::receipt);
                }
                MutationOutcome::Unknown { .. } => return Err(uncertain(command)),
                MutationOutcome::ClosedNeverApplied => {
                    return Err(SessionResourceError::conflict(
                        "work mutation was finalized without applying",
                    ))
                }
                MutationOutcome::NotApplied {
                    rejected_statement: Some(index),
                    ..
                } if retry_guards.contains(&index) => continue,
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
        Err(uncertain(command))
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
                self.work_resolution(command)
                    .await?
                    .ok_or_else(|| uncertain(command))
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

fn uncertain(command: &WorkCommand) -> SessionResourceError {
    SessionResourceError::persistence_uncertain(Some(command.session_id.clone()))
}

#[path = "session_work_journal.rs"]
mod journal;
use journal::owned_identity;

#[cfg(all(test, not(target_os = "emscripten")))]
#[path = "session_work_test.rs"]
mod tests;
