use super::*;
use crate::sessions::{work as journal, work_store};
use peri_acp_types::session_resources::work::{
    transition_work, EvidenceQuery, EvidenceRecord, EvidenceWrite, PayloadRef, WorkCommand,
    WorkInspection, WorkQuery, WorkReceipt, WorkResolution,
};

#[path = "work/execution.rs"]
pub(super) mod execution;
#[path = "work/offline.rs"]
pub(in crate::sessions::sqlite_store) mod offline;
use execution::{execute_plan, read_plan, rollback, sql_failure};

impl SqliteSessionData {
    pub(super) async fn read_work(
        &self,
        query: &WorkQuery,
    ) -> SessionResourceResult<WorkInspection> {
        query.validate()?;
        let mut transaction = self.database.pool.begin().await.map_err(sql_failure)?;
        let statements = work_store::inspection_plan(query)?;
        let rows = read_plan(&mut transaction, &statements).await?;
        let inspection = work_store::decode_inspection(query, &rows)?;
        transaction.commit().await.map_err(sql_failure)?;
        Ok(inspection)
    }

    pub(super) async fn load_evidence(
        &self,
        query: &EvidenceQuery,
    ) -> SessionResourceResult<EvidenceRecord> {
        let mut transaction = self.database.pool.begin().await.map_err(sql_failure)?;
        let statements = work_store::evidence_plan(query)?;
        let rows = read_plan(&mut transaction, &statements).await?;
        let evidence = work_store::decode_evidence(query, &rows)?;
        evidence.validate()?;
        transaction.commit().await.map_err(sql_failure)?;
        Ok(evidence)
    }

    pub(super) async fn store_evidence(
        &self,
        evidence: &EvidenceWrite,
    ) -> SessionResourceResult<PayloadRef> {
        self.writable()?;
        let (_, statements) = work_store::prepare_evidence_plan(evidence)?;
        let read_statement = work_store::payload::prepared_evidence_plan(evidence)?;
        let mut transaction = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql_failure)?;
        if let Err(error) = execute_plan(&mut transaction, &statements).await {
            return Err(rollback(transaction, error, &evidence.session_id).await);
        }
        let prepared = async {
            let rows = read_plan(&mut transaction, &[read_statement]).await?;
            work_store::payload::prepared_evidence_reference(evidence, &rows[0])
        }
        .await;
        let reference = match prepared {
            Ok(reference) => reference,
            Err(error) => return Err(rollback(transaction, error, &evidence.session_id).await),
        };
        transaction.commit().await.map_err(|error| {
            tracing::error!(%error, "SQLite immutable evidence commit outcome unknown");
            commit_failure(Some(evidence.session_id.clone()))
        })?;
        Ok(reference)
    }

    pub(super) async fn write_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        self.writable()?;
        command.digest()?;
        let mut transaction = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql_failure)?;
        let replay = match record_original(&mut transaction, command).await {
            Ok(replay) => replay,
            Err(error) => return Err(rollback(transaction, error, &command.session_id).await),
        };
        commit(transaction, command).await?;
        if let Some(resolution) = replay {
            return journal::receipt(resolution);
        }
        let mut transaction = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql_failure)?;
        let receipt = match apply_original(&mut transaction, command).await {
            Ok(receipt) => receipt,
            Err(error) => return Err(rollback(transaction, error, &command.session_id).await),
        };
        commit(transaction, command).await?;
        Ok(receipt)
    }

    pub(super) async fn resolve_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        self.writable()?;
        command.digest()?;
        let mut transaction = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql_failure)?;
        let resolution = match seal_original(&mut transaction, command).await {
            Ok(resolution) => resolution,
            Err(error) => return Err(rollback(transaction, error, &command.session_id).await),
        };
        commit(transaction, command).await?;
        Ok(resolution)
    }
}

async fn record_original(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
) -> SessionResourceResult<Option<WorkResolution>> {
    if let Some(resolution) = saved_resolution(connection, command).await? {
        acknowledge(connection, command).await?;
        return Ok(Some(resolution));
    }
    if let Some((json,)) = sqlx::query_as::<_, (String,)>(journal::READ_COMMAND)
        .bind(&command.mutation_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(sql_failure)?
    {
        if journal::original_command(&json)? != *command {
            return Err(SessionResourceError::conflict(
                "work original command identity conflicts",
            ));
        }
        return Err(commit_failure(Some(command.session_id.clone())));
    }
    execute_plan(connection, &journal::command_effects(command)?).await?;
    Ok(None)
}

async fn apply_original(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
) -> SessionResourceResult<WorkReceipt> {
    if let Some(resolution) = saved_resolution(connection, command).await? {
        acknowledge(connection, command).await?;
        return journal::receipt(resolution);
    }
    let statements = work_store::read_set(command)?;
    let rows = read_plan(connection, &statements).await?;
    let facts = work_store::decode_facts(command, &rows)?;
    let mut transition = transition_work(command, &facts)?;
    if let Some(statement) = work_store::response_validation_plan(command, &transition)? {
        let validation = read_plan(connection, &[statement]).await?;
        work_store::validate_response_transition(command, &mut transition, &validation[0])?;
    }
    let statements = work_store::sql_plan(command, &facts, &transition)?;
    execute_plan(connection, &statements).await?;
    sqlx::query(journal::INSERT_RECEIPT)
        .bind(&command.mutation_id)
        .bind(&command.session_id)
        .bind(command.digest()?)
        .bind(journal::encode(&WorkResolution::Applied {
            receipt: transition.receipt.clone(),
        })?)
        .execute(&mut *connection)
        .await
        .map_err(sql_failure)?;
    acknowledge(connection, command).await?;
    Ok(transition.receipt)
}

async fn seal_original(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
) -> SessionResourceResult<WorkResolution> {
    if let Some(resolution) = saved_resolution(connection, command).await? {
        acknowledge(connection, command).await?;
        return Ok(resolution);
    }
    execute_plan(connection, &journal::command_effects(command)?).await?;
    let resolution = WorkResolution::NotApplied;
    sqlx::query(journal::INSERT_RECEIPT)
        .bind(&command.mutation_id)
        .bind(&command.session_id)
        .bind(command.digest()?)
        .bind(journal::encode(&resolution)?)
        .execute(&mut *connection)
        .await
        .map_err(sql_failure)?;
    acknowledge(connection, command).await?;
    Ok(resolution)
}

async fn acknowledge(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
) -> SessionResourceResult<()> {
    let updated = sqlx::query(journal::ACK_COMMAND)
        .bind(&command.mutation_id)
        .bind(command.digest()?)
        .execute(connection)
        .await
        .map_err(sql_failure)?;
    if updated.rows_affected() != 1 {
        return Err(SessionResourceError::conflict(
            "work receipt acknowledgement guard failed",
        ));
    }
    Ok(())
}

async fn saved_resolution(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
) -> SessionResourceResult<Option<WorkResolution>> {
    let row: Option<(String, String)> = sqlx::query_as(journal::READ_RECEIPT)
        .bind(&command.mutation_id)
        .fetch_optional(connection)
        .await
        .map_err(sql_failure)?;
    row.map(|(digest, json)| journal::replay(command, &digest, &json))
        .transpose()
}

async fn commit(
    transaction: sqlx::Transaction<'_, sqlx::Sqlite>,
    command: &WorkCommand,
) -> SessionResourceResult<()> {
    transaction.commit().await.map_err(|error| {
        tracing::error!(%error, session_id = %command.session_id, mutation_id = %command.mutation_id, "SQLite work commit outcome unknown");
        commit_failure(Some(command.session_id.clone()))
    })
}

#[cfg(test)]
#[path = "work/work_test.rs"]
mod tests;
