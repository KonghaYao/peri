//! Canonical terminal reminder and explicit close intent writes.

use super::*;

impl SqliteSessionData {
    pub(super) async fn append_terminal_reminder(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.writable()?;
        let payload = PersistedPayload::SystemReminder {
            id: message_id,
            reminder: reminder.clone(),
        };
        let content = serialize_persisted_payload(&payload)
            .map_err(|_| corrupt("history entry is not serializable"))?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        self.assert_owner(&mut tx, id).await?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        let existing: Option<(String, String)> =
            sqlx::query_as("SELECT thread_id, content FROM messages WHERE message_id = ?1")
                .bind(message_id.as_uuid().to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        if let Some((owner, saved)) = existing {
            if owner != id.as_str() || saved != content {
                return Err(invalid_input(
                    "reminder id conflicts with another history entry",
                ));
            }
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(id.clone())))?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO messages (message_id, thread_id, role, content) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(message_id.as_uuid().to_string())
        .bind(id.as_str())
        .bind(payload_role(&payload))
        .bind(content)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        sqlx::query("UPDATE threads SET updated_at = ?1, message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2) WHERE id = ?2")
            .bind(peri_time::now_utc_rfc3339())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_sqlx(&e))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(true)
    }

    pub(super) async fn write_close_intent(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        self.assert_owner(&mut tx, id).await?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        sqlx::query(
            "INSERT OR IGNORE INTO session_close_intents(thread_id, requested_at) VALUES (?1, ?2)",
        )
        .bind(id.as_str())
        .bind(peri_time::now_utc_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

    pub(super) async fn read_close_intent(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        let table: Option<(String,)> = sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'session_close_intents'")
            .fetch_optional(&self.database.pool).await.map_err(|e| map_sqlx(&e))?;
        if table.is_none() {
            return Ok(false);
        }
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM session_close_intents WHERE thread_id = ?1")
                .bind(id.as_str())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|e| map_sqlx(&e))?;
        Ok(row.is_some())
    }
}
