use super::work::execution::{execute_plan, read_plan};

pub(in crate::sessions::sqlite_store) async fn guard_history_mutation(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> SessionResourceResult<()> {
    execute_plan(
        connection,
        &crate::sessions::work_store::history_guard(session_id),
    )
    .await
}
use super::*;
use crate::sessions::work_store::payload;
use peri_acp_types::session_resources::work::PayloadRef;
use peri_acp_types::store::deserialize_persisted_payload;

pub(in crate::sessions::sqlite_store) async fn tombstone_session(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> SessionResourceResult<()> {
    let row: Option<(String,)> = sqlx::query_as(crate::sessions::control::READ_STATE)
        .bind(session_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(super::work::execution::sql_failure)?;
    let current = crate::sessions::control::state(row.as_ref().map(|row| row.0.as_str()))?;
    execute_plan(
        connection,
        &crate::sessions::work_store::tombstone_plan(session_id, &current)?,
    )
    .await
}

pub(in crate::sessions::sqlite_store) async fn insert_payload(
    connection: &mut SqliteConnection,
    session_id: &str,
    value: &PersistedPayload,
) -> SessionResourceResult<()> {
    let (reference, statements) = payload::prepare(value, session_id)?;
    execute_plan(connection, &statements).await?;
    let reference = serde_json::to_string(&reference).map_err(|error| {
        tracing::error!(%error, %session_id, "SQLite message reference encoding failed");
        corrupt("canonical message reference cannot be encoded")
    })?;
    sqlx::query(
        "INSERT INTO messages(message_id,thread_id,role,content_ref,transcript_seq)
         VALUES (?1,?2,?3,?4,
          COALESCE((SELECT transcript_seq+1 FROM messages WHERE thread_id=?2 ORDER BY transcript_seq DESC LIMIT 1),0))",
    )
    .bind(value.id().as_uuid().to_string())
    .bind(session_id)
    .bind(payload_role(value))
    .bind(reference)
    .execute(connection)
    .await
    .map_err(super::work::execution::sql_failure)?;
    Ok(())
}

pub(in crate::sessions::sqlite_store) async fn decode_payload(
    connection: &mut SqliteConnection,
    message_id: &str,
    role: &str,
    content_ref: &str,
) -> SessionResourceResult<PersistedPayload> {
    let reference: PayloadRef = serde_json::from_str(content_ref).map_err(|error| {
        tracing::error!(%error, %message_id, "SQLite message reference decoding failed");
        corrupt("canonical message reference cannot be decoded")
    })?;
    let statements = [payload::read_statement(&reference)?];
    let rows = read_plan(connection, &statements).await?;
    let evidence = payload::decode_evidence(&reference, &rows[0]).map_err(|error| {
        tracing::error!(?error, %message_id, payload_id = %reference.payload_id, "SQLite canonical message evidence validation failed");
        error
    })?;
    let content = std::str::from_utf8(&evidence.bytes).map_err(|error| {
        tracing::error!(%error, %message_id, "SQLite canonical message is not UTF-8");
        corrupt("canonical message payload is not UTF-8")
    })?;
    let value = deserialize_persisted_payload(content).map_err(|error| {
        tracing::error!(%error, %message_id, "SQLite canonical message decoding failed");
        corrupt("canonical message payload is not readable")
    })?;
    if value.id().as_uuid().to_string() != message_id || payload_role(&value) != role {
        tracing::error!(%message_id, %role, stored_id = %value.id().as_uuid(), stored_role = payload_role(&value), "SQLite canonical message envelope conflicts with its payload");
        return Err(corrupt(
            "canonical message envelope does not match its payload",
        ));
    }
    Ok(value)
}

pub(in crate::sessions::sqlite_store) async fn decode_rows(
    connection: &mut SqliteConnection,
    rows: Vec<(String, String, String)>,
) -> SessionResourceResult<Vec<PersistedPayload>> {
    let mut payloads = Vec::with_capacity(rows.len());
    for (message_id, role, reference) in rows {
        payloads.push(decode_payload(connection, &message_id, &role, &reference).await?);
    }
    Ok(payloads)
}

#[cfg(test)]
#[path = "messages_test.rs"]
mod tests;
