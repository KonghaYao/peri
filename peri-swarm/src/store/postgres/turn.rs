//! Turn, input-receipt and environment-binding halves of [`SwarmStore`].
//!
//! Input receipts are insert-if-absent (`ON CONFLICT DO NOTHING`): the caller
//! compares the stored fingerprint to decide between a replay and a
//! `409 input_conflict`. Turns are upserted so the terminal write replaces the
//! status, times, error and usage while keeping the opening `created_at`.

use serde_json::Value;

use super::budget::{unexpected_row_count, unexplained, within_budget};
use super::codec::{
    bind_binding, bind_input, bind_turn, bind_turn_usage, decode_binding, decode_input,
    decode_turn, decode_turn_usage, encode,
};
use super::sql::{
    CLOSE_OPEN_TURNS_SQL, HAS_OPEN_TURN_SQL, INSERT_INPUT_SQL, RECORD_TURN_USAGE_SQL,
    SELECT_BINDING_SQL, SELECT_INPUT_SQL, SELECT_SESSION_USAGE_TURNS_SQL,
    SELECT_TURNS_ASC_AFTER_SQL, SELECT_TURNS_ASC_SQL, SELECT_TURNS_DESC_AFTER_SQL,
    SELECT_TURNS_DESC_SQL, SELECT_TURN_SQL, SELECT_TURN_USAGE_WINDOW_SQL, UPSERT_BINDING_SQL,
    UPSERT_TURN_SQL,
};
use super::SwarmStateStore;
use crate::domain::{ItemOrder, TurnStatus, TurnsWindow};
use crate::store::{
    InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmTurnRow, SwarmTurnUsageRow,
};

impl SwarmStateStore {
    pub(super) async fn put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError> {
        let affected = within_budget(
            "put_input",
            bind_input(sqlx::query(INSERT_INPUT_SQL), &row).execute(self.pool()),
        )
        .await?
        .rows_affected();
        match affected {
            1 => Ok(InputPut::Inserted),
            0 => {
                let existing = self
                    .get_input(&row.logical_id, &row.input_id)
                    .await?
                    .ok_or_else(|| {
                        StoreError::Unavailable(
                            "put_input: conflicting input receipt is not readable".to_owned(),
                        )
                    })?;
                Ok(InputPut::Existing(existing))
            }
            other => Err(unexplained("put_input", other)),
        }
    }

    pub(super) async fn get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<SwarmInputRow>, StoreError> {
        let rows = within_budget(
            "get_input",
            sqlx::query(SELECT_INPUT_SQL)
                .bind(logical_id.to_owned())
                .bind(input_id.to_owned())
                .fetch_all(self.pool()),
        )
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_input(row).map(Some),
            rows => Err(unexpected_row_count("get_input", rows.len())),
        }
    }

    pub(super) async fn put_binding(&self, row: SwarmBindingRow) -> Result<(), StoreError> {
        let affected = within_budget(
            "put_binding",
            bind_binding(sqlx::query(UPSERT_BINDING_SQL), &row).execute(self.pool()),
        )
        .await?
        .rows_affected();
        match affected {
            1 => Ok(()),
            other => Err(unexplained("put_binding", other)),
        }
    }

    pub(super) async fn get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<SwarmBindingRow>, StoreError> {
        let rows = within_budget(
            "get_binding",
            sqlx::query(SELECT_BINDING_SQL)
                .bind(logical_env_id.to_owned())
                .fetch_all(self.pool()),
        )
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_binding(row).map(Some),
            rows => Err(unexpected_row_count("get_binding", rows.len())),
        }
    }

    pub(super) async fn upsert_turn(&self, row: SwarmTurnRow) -> Result<(), StoreError> {
        let affected = within_budget(
            "upsert_turn",
            bind_turn(sqlx::query(UPSERT_TURN_SQL), &row)?.execute(self.pool()),
        )
        .await?
        .rows_affected();
        match affected {
            1 => Ok(()),
            other => Err(unexplained("upsert_turn", other)),
        }
    }

    pub(super) async fn record_turn_usage(
        &self,
        logical_id: &str,
        turn_id: &str,
        created_at: i64,
        started_at: Option<i64>,
        usage: &Value,
    ) -> Result<(), StoreError> {
        let affected = within_budget(
            "record_turn_usage",
            bind_turn_usage(
                sqlx::query(RECORD_TURN_USAGE_SQL),
                logical_id,
                turn_id,
                created_at,
                started_at,
                usage,
            )?
            .execute(self.pool()),
        )
        .await?
        .rows_affected();
        // Zero rows means the turn is already terminal and kept its own usage;
        // that is the documented no-op, not a lost write.
        match affected {
            0 | 1 => Ok(()),
            other => Err(unexplained("record_turn_usage", other)),
        }
    }

    pub(super) async fn get_turn(
        &self,
        logical_id: &str,
        turn_id: &str,
    ) -> Result<Option<SwarmTurnRow>, StoreError> {
        let rows = within_budget(
            "get_turn",
            sqlx::query(SELECT_TURN_SQL)
                .bind(logical_id.to_owned())
                .bind(turn_id.to_owned())
                .fetch_all(self.pool()),
        )
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_turn(row).map(Some),
            rows => Err(unexpected_row_count("get_turn", rows.len())),
        }
    }

    pub(super) async fn turn_page(
        &self,
        logical_id: &str,
        window: &TurnsWindow,
    ) -> Result<Vec<SwarmTurnRow>, StoreError> {
        // The service rejects an unknown cursor; a missing row here simply
        // selects nothing.
        let after = match window.after.as_deref() {
            Some(after) => self.get_turn(logical_id, after).await?,
            None => None,
        };
        let probe = i64::from(window.limit.saturating_add(1));
        let rows = match (window.order, after) {
            (ItemOrder::Asc, Some(after)) => {
                within_budget(
                    "turn_page",
                    sqlx::query(SELECT_TURNS_ASC_AFTER_SQL)
                        .bind(logical_id.to_owned())
                        .bind(after.created_at)
                        .bind(after.turn_id)
                        .bind(probe)
                        .fetch_all(self.pool()),
                )
                .await?
            }
            (ItemOrder::Asc, None) => {
                within_budget(
                    "turn_page",
                    sqlx::query(SELECT_TURNS_ASC_SQL)
                        .bind(logical_id.to_owned())
                        .bind(probe)
                        .fetch_all(self.pool()),
                )
                .await?
            }
            (ItemOrder::Desc, Some(after)) => {
                within_budget(
                    "turn_page",
                    sqlx::query(SELECT_TURNS_DESC_AFTER_SQL)
                        .bind(logical_id.to_owned())
                        .bind(after.created_at)
                        .bind(after.turn_id)
                        .bind(probe)
                        .fetch_all(self.pool()),
                )
                .await?
            }
            (ItemOrder::Desc, None) => {
                within_budget(
                    "turn_page",
                    sqlx::query(SELECT_TURNS_DESC_SQL)
                        .bind(logical_id.to_owned())
                        .bind(probe)
                        .fetch_all(self.pool()),
                )
                .await?
            }
        };
        rows.iter().map(decode_turn).collect()
    }

    pub(super) async fn session_usage_turns(
        &self,
        logical_id: &str,
        limit: u32,
    ) -> Result<Vec<SwarmTurnUsageRow>, StoreError> {
        let rows = within_budget(
            "session_usage_turns",
            sqlx::query(SELECT_SESSION_USAGE_TURNS_SQL)
                .bind(logical_id.to_owned())
                .bind(i64::from(limit))
                .fetch_all(self.pool()),
        )
        .await?;
        rows.iter().map(decode_turn_usage).collect()
    }

    pub(super) async fn turn_usage_window(
        &self,
        start_at: i64,
        end_at: i64,
        limit: u32,
    ) -> Result<Vec<SwarmTurnUsageRow>, StoreError> {
        let rows = within_budget(
            "turn_usage_window",
            sqlx::query(SELECT_TURN_USAGE_WINDOW_SQL)
                .bind(start_at)
                .bind(end_at)
                .bind(i64::from(limit))
                .fetch_all(self.pool()),
        )
        .await?;
        rows.iter().map(decode_turn_usage).collect()
    }

    pub(super) async fn close_open_turns(
        &self,
        logical_id: &str,
        status: TurnStatus,
        completed_at: i64,
        error: Option<serde_json::Value>,
    ) -> Result<u64, StoreError> {
        let error = match error {
            Some(error) => Some(encode(&error, "turn error")?),
            None => None,
        };
        let affected = within_budget(
            "close_open_turns",
            sqlx::query(CLOSE_OPEN_TURNS_SQL)
                .bind(logical_id.to_owned())
                .bind(status.as_str())
                .bind(completed_at)
                .bind(error)
                .execute(self.pool()),
        )
        .await?
        .rows_affected();
        Ok(affected)
    }

    pub(super) async fn has_open_turn(&self, logical_id: &str) -> Result<bool, StoreError> {
        let open: bool = within_budget(
            "has_open_turn",
            sqlx::query_scalar(HAS_OPEN_TURN_SQL)
                .bind(logical_id.to_owned())
                .fetch_one(self.pool()),
        )
        .await?;
        Ok(open)
    }
}
