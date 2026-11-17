//! Canonical terminal reminder and explicit close intent writes.

use super::{
    MessageId, PersistedPayload, SessionResourceResult, SqliteConnection, SqliteSessionData,
    ThreadId, TrustedSystemReminder,
};
use crate::sessions::sqlite_store::session_data::thread_exists_on;

impl SqliteSessionData {
    pub(super) async fn append_terminal_reminder(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| super::map_sqlx(&e))?;
        self.require_session(&mut tx, id).await?;
        let appended = self
            .append_terminal_reminder_on(&mut tx, id, message_id, reminder)
            .await?;
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(id.clone())))?;
        Ok(appended)
    }

    /// 同一份终态提醒写入的事务内形态：关闭结算与单条幂等写入共用一处判定。
    pub(super) async fn append_terminal_reminder_on(
        &self,
        tx: &mut SqliteConnection,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        let payload = PersistedPayload::SystemReminder {
            id: message_id,
            reminder: reminder.clone(),
        };
        let content = super::serialize_persisted_payload(&payload)
            .map_err(|_| super::corrupt("history entry is not serializable"))?;
        if !thread_exists_on(&mut *tx, id)
            .await
            .map_err(super::read_failure)?
        {
            return Err(super::not_found());
        }
        let existing: Option<(String, String)> =
            sqlx::query_as("SELECT thread_id, content FROM messages WHERE message_id = ?1")
                .bind(message_id.as_uuid().to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| super::map_sqlx(&error))?;
        if let Some((owner, saved)) = existing {
            if owner != id.as_str() || saved != content {
                return Err(super::invalid_input(&format!(
                    "reminder id {} conflicts with another history entry",
                    message_id.as_uuid()
                )));
            }
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO messages (message_id, thread_id, role, content) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(message_id.as_uuid().to_string())
        .bind(id.as_str())
        .bind(super::payload_role(&payload))
        .bind(content)
        .execute(&mut *tx)
        .await
        .map_err(|e| super::map_sqlx(&e))?;
        sqlx::query("UPDATE threads SET updated_at = ?1, message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2) WHERE id = ?2")
            .bind(peri_time::now_utc_rfc3339())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| super::map_sqlx(&e))?;
        Ok(true)
    }

    pub(super) async fn write_close_intent(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| super::map_sqlx(&e))?;
        self.require_session(&mut tx, id).await?;
        if !thread_exists_on(&mut tx, id)
            .await
            .map_err(super::read_failure)?
        {
            return Err(super::not_found());
        }
        sqlx::query(
            "INSERT OR IGNORE INTO session_close_intents(thread_id, requested_at) VALUES (?1, ?2)",
        )
        .bind(id.as_str())
        .bind(peri_time::now_utc_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(|e| super::map_sqlx(&e))?;
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(id.clone())))?;
        Ok(())
    }

    pub(super) async fn read_close_intent(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        let table: Option<(String,)> = sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'session_close_intents'")
            .fetch_optional(&self.database.pool).await.map_err(|e| super::map_sqlx(&e))?;
        if table.is_none() {
            return Ok(false);
        }
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM session_close_intents WHERE thread_id = ?1")
                .bind(id.as_str())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|e| super::map_sqlx(&e))?;
        Ok(row.is_some())
    }
}
