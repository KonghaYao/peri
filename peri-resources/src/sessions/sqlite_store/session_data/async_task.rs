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
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        self.require_session(&mut tx, id).await?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        let existing: Option<(String, String, String)> = sqlx::query_as(
            "SELECT thread_id, role, content_ref FROM messages WHERE message_id = ?1",
        )
        .bind(message_id.as_uuid().to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        if let Some((owner, role, reference)) = existing {
            if owner != id.as_str() {
                return Err(invalid_input("reminder id belongs to another session"));
            }
            let saved = messages::decode_payload(
                &mut tx,
                &message_id.as_uuid().to_string(),
                &role,
                &reference,
            )
            .await?;
            if peri_acp_types::store::serialize_persisted_payload(&saved)
                .map_err(|_| corrupt("stored reminder is not serializable"))?
                != peri_acp_types::store::serialize_persisted_payload(&payload)
                    .map_err(|_| corrupt("reminder is not serializable"))?
            {
                return Err(invalid_input(
                    "reminder id conflicts with another history entry",
                ));
            }
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(id.clone())))?;
            return Ok(false);
        }
        messages::insert_payload(&mut tx, id, &payload).await?;
        sqlx::query(
            "UPDATE threads SET updated_at = ?1, message_count = message_count + 1 WHERE id = ?2",
        )
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
        let current = self.read_control(id).await?;
        if current.status == peri_acp_types::session_resources::ControlStatus::Closing {
            return Ok(());
        }
        let command = crate::sessions::control::close_command(
            id,
            &current,
            peri_acp_types::session_resources::ControlAction::Close,
        )?;
        let receipt = self.write_control(&command).await?;
        if receipt.decision != peri_acp_types::session_resources::ControlDecision::Accepted {
            return Err(SessionResourceError::conflict(
                "session close control was rejected",
            ));
        }
        Ok(())
    }

    pub(super) async fn read_close_intent(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        Ok(self.read_control(id).await?.status
            == peri_acp_types::session_resources::ControlStatus::Closing)
    }
}
