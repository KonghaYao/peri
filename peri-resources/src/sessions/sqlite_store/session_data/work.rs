use super::*;
use crate::sessions::work;
use peri_acp_types::session_resources::work::{
    reduce_work, WorkCommand, WorkQuery, WorkReceipt, WorkResolution, WorkSnapshot,
};

impl SqliteSessionData {
    pub(super) async fn read_work_command(
        &self,
        query: &peri_acp_types::session_resources::work::WorkCommandQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::OwnedWorkCommand>>
    {
        let row: Option<(String, String, Option<String>, bool)> =
            sqlx::query_as(work::READ_OWNED_COMMAND)
                .bind(&query.mutation_id)
                .bind(&query.session_id)
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|error| map_sqlx(&error))?;
        row.map(|(json, digest, resolution, reconciled)| {
            work::owned_command(&json, &digest, resolution.as_deref(), reconciled)
        })
        .transpose()
    }
    pub(super) async fn read_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkSnapshot> {
        let mut tx = self
            .database
            .pool
            .begin()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let (control, state, _) = read_snapshot(&mut tx, &query.session_id).await?;
        let mut snapshot = WorkSnapshot::from_state(query, control, state);
        let rows: Vec<(String,)> = sqlx::query_as(work::READ_PENDING)
            .bind(&query.session_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        snapshot.pending_commands = rows
            .into_iter()
            .map(|row| work::original_command(&row.0))
            .collect::<SessionResourceResult<_>>()?;
        if !snapshot.pending_commands.is_empty() {
            snapshot.blocked = true;
            snapshot.candidates.clear();
        }
        Ok(snapshot)
    }

    pub(super) async fn write_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        self.writable()?;
        command.digest()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        if let Some(resolution) = saved_resolution(&mut tx, command).await? {
            acknowledge(&mut tx, command).await?;
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
            return work::receipt(resolution);
        }
        if let Some((json,)) = sqlx::query_as::<_, (String,)>(work::READ_COMMAND)
            .bind(&command.mutation_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?
        {
            if work::original_command(&json)? != *command {
                return Err(SessionResourceError::conflict(
                    "work original command identity conflicts",
                ));
            }
            return Err(commit_failure(Some(command.session_id.clone())));
        }
        for effect in work::command_effects(command)? {
            execute_effect(&mut tx, effect).await?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let (control, state, state_json) = read_snapshot(&mut tx, &command.session_id).await?;
        let reduction = reduce_work(command, &control, &state)?;
        for effect in work::mutation_effects(command, &state, state_json, &control, &reduction)? {
            let mut query = sqlx::query(effect.sql);
            for value in effect.params {
                query = query.bind(value);
            }
            query
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        acknowledge(&mut tx, command).await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
        Ok(reduction.receipt)
    }

    pub(super) async fn resolve_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        self.writable()?;
        let digest = command.digest()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        if let Some(resolution) = saved_resolution(&mut tx, command).await? {
            acknowledge(&mut tx, command).await?;
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
            return Ok(resolution);
        }
        for effect in work::command_effects(command)? {
            execute_effect(&mut tx, effect).await?;
        }
        let resolution = WorkResolution::NotApplied;
        sqlx::query(work::INSERT_RECEIPT)
            .bind(&command.mutation_id)
            .bind(&command.session_id)
            .bind(digest)
            .bind(work::encode(&resolution)?)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        acknowledge(&mut tx, command).await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
        Ok(resolution)
    }
}

async fn execute_effect(
    connection: &mut SqliteConnection,
    effect: work::WorkEffect,
) -> SessionResourceResult<()> {
    let mut query = sqlx::query(effect.sql);
    for value in effect.params {
        query = query.bind(value);
    }
    query
        .execute(connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    Ok(())
}

async fn acknowledge(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
) -> SessionResourceResult<()> {
    sqlx::query(work::ACK_COMMAND)
        .bind(&command.mutation_id)
        .bind(command.digest()?)
        .execute(connection)
        .await
        .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
    Ok(())
}

async fn saved_resolution(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
) -> SessionResourceResult<Option<WorkResolution>> {
    let row: Option<(String, String)> = sqlx::query_as(work::READ_RECEIPT)
        .bind(&command.mutation_id)
        .fetch_optional(connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    row.map(|(digest, json)| work::replay(command, &digest, &json))
        .transpose()
}

async fn read_snapshot(
    connection: &mut SqliteConnection,
    id: &str,
) -> SessionResourceResult<(
    peri_acp_types::session_resources::ControlState,
    peri_acp_types::session_resources::work::WorkState,
    Option<String>,
)> {
    let control: Option<(String,)> = sqlx::query_as(crate::sessions::control::READ_STATE)
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    let state: Option<(String,)> = sqlx::query_as(work::READ_STATE)
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    let exists = thread_exists_on(connection, &id.to_owned())
        .await
        .map_err(read_failure)?;
    if !exists && control.is_none() && state.is_none() {
        return Err(not_found());
    }
    let has_history: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages WHERE thread_id=?1)")
            .bind(id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
    Ok((
        crate::sessions::control::state(control.as_ref().map(|row| row.0.as_str()))?,
        work::state(state.as_ref().map(|row| row.0.as_str()), has_history)?,
        state.map(|row| row.0),
    ))
}
