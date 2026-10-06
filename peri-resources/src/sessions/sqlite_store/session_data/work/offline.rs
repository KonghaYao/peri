use super::*;
use crate::sessions::work_store::migration::{self, StoppedWriterApproval, WorkMigrationReport};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;

const PAGE_SIZE: i64 = 64;
const WORK_PAGE: &str = "SELECT original.session_id,original.session_id,original.state_json,COALESCE(json_extract(control.state_json,'$.lifecycle'),1),0 FROM session_work_state original LEFT JOIN session_control_state control ON control.session_id=original.session_id WHERE (?1 IS NULL OR original.session_id>?1) ORDER BY original.session_id LIMIT ?2";
const COMMAND_PAGE: &str = "SELECT original.mutation_id,original.session_id,original.command_json,original.digest,COALESCE(json_extract(control.state_json,'$.lifecycle'),1),receipt.resolution_json,receipt.digest FROM session_work_commands original LEFT JOIN session_control_state control ON control.session_id=original.session_id LEFT JOIN session_work_receipts receipt ON receipt.mutation_id=original.mutation_id AND receipt.session_id=original.session_id WHERE original.kind='mutation' AND (?1 IS NULL OR original.mutation_id>?1) ORDER BY original.mutation_id LIMIT ?2";
const RECEIPT_PAGE: &str = "SELECT receipt.mutation_id,receipt.session_id,original.command_json,original.digest,COALESCE(json_extract(control.state_json,'$.lifecycle'),1),receipt.resolution_json,receipt.digest FROM session_work_receipts receipt LEFT JOIN session_control_state control ON control.session_id=receipt.session_id LEFT JOIN session_work_commands original ON original.mutation_id=receipt.mutation_id AND original.session_id=receipt.session_id AND original.kind='mutation' WHERE (?1 IS NULL OR receipt.mutation_id>?1) ORDER BY receipt.mutation_id LIMIT ?2";
const EVENT_PAGE: &str = "SELECT event_key,?3,event_json,1,0 FROM session_work_events WHERE (?1 IS NULL OR event_key>?1) ORDER BY event_key LIMIT ?2";

pub async fn migrate_stopped_work_store(
    path: impl AsRef<Path>,
    approval: &StoppedWriterApproval,
) -> SessionResourceResult<WorkMigrationReport> {
    let initialization = migration::initialization_plan(approval)?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path.as_ref())
                .create_if_missing(false),
        )
        .await
        .map_err(sql_failure)?;
    let outcome = async {
        let mut transaction = pool
            .begin_with("BEGIN EXCLUSIVE")
            .await
            .map_err(sql_failure)?;
        let report = match migrate_connection(&mut transaction, &initialization).await {
            Ok(report) => report,
            Err(error) => return Err(rollback(transaction, error, "offline-work-migration").await),
        };
        transaction.commit().await.map_err(|error| {
            tracing::error!(%error, "SQLite offline Work migration commit outcome unknown");
            commit_failure(None)
        })?;
        Ok(report)
    }
    .await;
    pool.close().await;
    outcome
}

async fn migrate_connection(
    connection: &mut SqliteConnection,
    initialization: &[work_store::SqlStatement],
) -> SessionResourceResult<WorkMigrationReport> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .map_err(sql_failure)?;
    if version != 17 {
        return Err(invalid_input(
            "offline Work migration only accepts schema 17",
        ));
    }
    execute_plan(connection, initialization).await?;
    execute_plan(connection, &migration::message_columns_plan()).await?;
    execute_plan(connection, &migration::archive_scope_plan()).await?;
    let mut report = WorkMigrationReport::default();
    import_messages(connection, &mut report).await?;
    for (kind, statement) in [("work", WORK_PAGE), ("event", EVENT_PAGE)] {
        import_blobs(connection, kind, statement, &mut report).await?;
    }
    for (kind, statement) in [("command", COMMAND_PAGE), ("receipt", RECEIPT_PAGE)] {
        import_journal(connection, kind, statement, &mut report).await?;
    }
    execute_plan(connection, &migration::finalize_plan()).await?;
    let sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(DISTINCT session_id) FROM session_work_head")
            .fetch_one(&mut *connection)
            .await
            .map_err(sql_failure)?;
    report.sessions = nonnegative(sessions)?;
    let unknown: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT session_id) FROM session_work_head WHERE json_extract(record_json,'$.legacyUnknown')>0")
        .fetch_one(&mut *connection).await.map_err(sql_failure)?;
    report.legacy_unknown_sessions = nonnegative(unknown)?;
    sqlx::query("PRAGMA user_version=17")
        .execute(connection)
        .await
        .map_err(sql_failure)?;
    Ok(report)
}

async fn import_messages(
    connection: &mut SqliteConnection,
    report: &mut WorkMigrationReport,
) -> SessionResourceResult<()> {
    let mut cursor: Option<i64> = None;
    loop {
        let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
            "SELECT rowid,message_id,thread_id,role,content FROM messages WHERE (?1 IS NULL OR rowid>?1) ORDER BY rowid LIMIT ?2",
        ).bind(cursor).bind(PAGE_SIZE).fetch_all(&mut *connection).await.map_err(sql_failure)?;
        if rows.is_empty() {
            break;
        }
        for (row_id, message_id, session_id, role, content) in rows {
            let statements = migration::import_message(
                &message_id,
                &session_id,
                &role,
                report.messages,
                &content,
            )?;
            execute_plan(connection, &statements).await?;
            increment(&mut report.messages)?;
            cursor = Some(row_id);
        }
    }
    Ok(())
}

async fn import_blobs(
    connection: &mut SqliteConnection,
    kind: &str,
    statement: &'static str,
    report: &mut WorkMigrationReport,
) -> SessionResourceResult<()> {
    let mut cursor: Option<String> = None;
    loop {
        let query = sqlx::query_as::<_, (String, String, String, i64, bool)>(statement)
            .bind(cursor.as_deref())
            .bind(PAGE_SIZE);
        let query = if kind == "event" {
            query.bind(migration::LEGACY_EVENT_SCOPE)
        } else {
            query
        };
        let rows = query
            .fetch_all(&mut *connection)
            .await
            .map_err(sql_failure)?;
        if rows.is_empty() {
            break;
        }
        for (source_id, session_id, json, lifecycle, unresolved) in rows {
            let lifecycle = nonnegative(lifecycle)?;
            let unresolved = if kind == "work" {
                migration::classify_legacy(&json)?
            } else {
                unresolved
            };
            let statements = migration::import_blob(
                &session_id,
                lifecycle,
                kind,
                &source_id,
                json.as_bytes(),
                unresolved,
            )?;
            execute_plan(connection, &statements).await?;
            if kind == "work" {
                execute_plan(
                    connection,
                    &migration::import_head(&session_id, lifecycle, &json, unresolved)?,
                )
                .await?;
            }
            increment(&mut report.retained_evidence)?;
            cursor = Some(source_id);
        }
    }
    Ok(())
}

async fn import_journal(
    connection: &mut SqliteConnection,
    kind: &str,
    statement: &'static str,
    report: &mut WorkMigrationReport,
) -> SessionResourceResult<()> {
    let mut cursor: Option<String> = None;
    loop {
        let rows: Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(statement)
            .bind(cursor.as_deref())
            .bind(PAGE_SIZE)
            .fetch_all(&mut *connection)
            .await
            .map_err(sql_failure)?;
        if rows.is_empty() {
            break;
        }
        for (
            mutation_id,
            session_id,
            command_json,
            command_digest,
            lifecycle,
            receipt_json,
            receipt_digest,
        ) in rows
        {
            let unresolved = if let (Some(command), Some(digest)) = (&command_json, &command_digest)
            {
                migration::legacy_command_unresolved(
                    &session_id,
                    &mutation_id,
                    command,
                    digest,
                    receipt_digest.as_deref(),
                    receipt_json.as_deref(),
                )?
            } else {
                true
            };
            let json = if kind == "command" {
                command_json
            } else {
                receipt_json
            }
            .ok_or_else(|| corrupt("offline migration source journal bytes are missing"))?;
            execute_plan(
                connection,
                &migration::import_blob(
                    &session_id,
                    nonnegative(lifecycle)?,
                    kind,
                    &mutation_id,
                    json.as_bytes(),
                    unresolved,
                )?,
            )
            .await?;
            increment(&mut report.retained_evidence)?;
            cursor = Some(mutation_id);
        }
    }
    Ok(())
}

fn nonnegative(value: i64) -> SessionResourceResult<u64> {
    u64::try_from(value).map_err(|_| corrupt("offline migration encountered a negative counter"))
}

fn increment(value: &mut u64) -> SessionResourceResult<()> {
    *value = value
        .checked_add(1)
        .ok_or_else(|| corrupt("offline migration counter overflow"))?;
    Ok(())
}
