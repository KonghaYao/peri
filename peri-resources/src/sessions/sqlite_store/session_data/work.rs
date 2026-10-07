use super::*;
use crate::sessions::work;
use peri_acp_types::session_resources::work::{
    reduce_work, DeliveryRecord, WorkCommand, WorkDeliveryQuery, WorkQuery, WorkReceipt,
    WorkResolution, WorkSnapshot,
};

type ResourceOwnerSqliteRow = (
    bool,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

impl SqliteSessionData {
    pub(super) async fn read_resource_owner_facts(
        &self,
        id: &ThreadId,
        previous_lifecycle: u64,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::ResourceOwnerFacts> {
        let row: ResourceOwnerSqliteRow = sqlx::query_as(work::READ_RESOURCE_OWNER_FACTS)
            .bind(id)
            .bind(previous_lifecycle.to_string())
            .fetch_one(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        work::resource_owner_facts(work::ResourceOwnerRow {
            session_exists: row.0,
            control_json: row.1.as_deref(),
            state_exists: row.2,
            revision_json: row.3.as_deref(),
            current_owner_json: row.4.as_deref(),
            previous_owner_json: row.5.as_deref(),
            current_child_json: row.6.as_deref(),
            previous_child_json: row.7.as_deref(),
            owners_type: row.8.as_deref(),
            child_metadata_type: row.9.as_deref(),
        })
    }
    pub(super) async fn read_work_revision(&self, id: &ThreadId) -> SessionResourceResult<u64> {
        let (session_exists, control_exists, state_exists, revision_json): (
            i64,
            i64,
            i64,
            Option<String>,
        ) = sqlx::query_as(work::READ_REVISION)
            .bind(id)
            .fetch_one(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        work::revision(
            session_exists,
            control_exists,
            state_exists,
            revision_json.as_deref(),
        )
    }

    pub(super) async fn read_work_availability(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkAvailability> {
        let (exists, control, facts, history): (bool, Option<String>, Option<String>, bool) =
            sqlx::query_as(work::READ_AVAILABILITY)
                .bind(id)
                .fetch_one(&self.database.pool)
                .await
                .map_err(|error| map_sqlx(&error))?;
        work::availability(exists, control.as_deref(), facts.as_deref(), history)
    }

    pub(super) async fn read_delivery(
        &self,
        query: &WorkDeliveryQuery,
    ) -> SessionResourceResult<Option<DeliveryRecord>> {
        let (exists, kind, json): (bool, Option<String>, Option<String>) =
            sqlx::query_as(work::READ_DELIVERY)
                .bind(&query.session_id)
                .bind(&query.delivery_id)
                .fetch_one(&self.database.pool)
                .await
                .map_err(|error| map_sqlx(&error))?;
        if !exists {
            return Err(not_found());
        }
        work::delivery(query, kind.as_deref(), json.as_deref())
    }

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
        let digest = command.digest()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        if let Some(resolution) = saved_resolution(&mut tx, command, &digest).await? {
            acknowledge(&mut tx, command, &digest).await?;
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
        for effect in work::command_effects_with_digest(command, &digest)? {
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
        let initial_json = work::pre_state_json(state_json, &state)?;
        let parent_command = work::terminal_parent_command(command, &state);
        let reduction = reduce_work(command, &control, state)?;
        for effect in work::mutation_effects(
            command,
            &digest,
            initial_json,
            parent_command.as_ref(),
            &control,
            &reduction,
        )? {
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
        acknowledge(&mut tx, command, &digest).await?;
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
        if let Some(resolution) = saved_resolution(&mut tx, command, &digest).await? {
            acknowledge(&mut tx, command, &digest).await?;
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
            return Ok(resolution);
        }
        for effect in work::command_effects_with_digest(command, &digest)? {
            execute_effect(&mut tx, effect).await?;
        }
        let resolution = WorkResolution::NotApplied;
        sqlx::query(work::INSERT_RECEIPT)
            .bind(&command.mutation_id)
            .bind(&command.session_id)
            .bind(&digest)
            .bind(work::encode(&resolution)?)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        acknowledge(&mut tx, command, &digest).await?;
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
    digest: &str,
) -> SessionResourceResult<()> {
    sqlx::query(work::ACK_COMMAND)
        .bind(&command.mutation_id)
        .bind(digest)
        .execute(connection)
        .await
        .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
    Ok(())
}

async fn saved_resolution(
    connection: &mut SqliteConnection,
    command: &WorkCommand,
    digest: &str,
) -> SessionResourceResult<Option<WorkResolution>> {
    let row: Option<(String, String)> = sqlx::query_as(work::READ_RECEIPT)
        .bind(&command.mutation_id)
        .fetch_optional(connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    row.map(|(stored_digest, json)| {
        work::replay_with_digest(command, digest, &stored_digest, &json)
    })
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
