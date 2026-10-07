use super::*;
use crate::sessions::control;
use peri_acp_types::session_resources::{
    control::decide_control, ControlAction, ControlCommand, ControlReceipt, ControlResolution,
    ControlState,
};

impl SqliteSessionData {
    pub(super) async fn read_control(&self, id: &ThreadId) -> SessionResourceResult<ControlState> {
        let mut tx = self
            .database
            .pool
            .begin()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let row: Option<(String,)> = sqlx::query_as(control::READ_STATE)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if row.is_none() && !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        control::state(row.as_ref().map(|row| row.0.as_str()))
    }

    pub(super) async fn write_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlReceipt> {
        self.writable()?;
        let digest = command.digest()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let prior: Option<(String, String)> = sqlx::query_as(control::READ_RECEIPT)
            .bind(&command.command_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if let Some((saved_digest, json)) = prior {
            return control::receipt(control::replay(command, &saved_digest, &json)?);
        }
        let row: Option<(String,)> = sqlx::query_as(control::READ_STATE)
            .bind(&command.session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if !thread_exists_on(&mut tx, &command.session_id)
            .await
            .map_err(read_failure)?
            && !(row.is_some() && command.action == ControlAction::FinishClose)
        {
            return Err(not_found());
        }
        let current = control::state(row.as_ref().map(|row| row.0.as_str()))?;
        let receipt = decide_control(command, &current);
        sqlx::query(control::INSERT_STATE)
            .bind(&command.session_id)
            .bind(control::encode(&current)?)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        sqlx::query(control::UPDATE_STATE)
            .bind(&command.session_id)
            .bind(control::encode(&receipt.state)?)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        match control::closing_projection(command, &receipt) {
            Some(true) => {
                sqlx::query(control::MARK_CLOSING)
                    .bind(&command.session_id)
                    .bind(peri_time::now_utc_rfc3339())
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            }
            Some(false) => {
                sqlx::query(control::CLEAR_CLOSING)
                    .bind(&command.session_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            }
            None => {}
        }
        let resolution = ControlResolution::Applied {
            receipt: receipt.clone(),
        };
        sqlx::query(control::INSERT_RECEIPT)
            .bind(&command.command_id)
            .bind(&command.session_id)
            .bind(digest)
            .bind(control::encode(&resolution)?)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
        Ok(receipt)
    }

    pub(super) async fn resolve_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlResolution> {
        self.writable()?;
        let digest = command.digest()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let prior: Option<(String, String)> = sqlx::query_as(control::READ_RECEIPT)
            .bind(&command.command_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if let Some((saved_digest, json)) = prior {
            return control::replay(command, &saved_digest, &json);
        }
        sqlx::query(control::INSERT_RECEIPT)
            .bind(&command.command_id)
            .bind(&command.session_id)
            .bind(digest)
            .bind(control::encode(&ControlResolution::NotApplied)?)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(command.session_id.clone())))?;
        Ok(ControlResolution::NotApplied)
    }
}
