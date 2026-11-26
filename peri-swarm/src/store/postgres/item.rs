//! Item half of [`SwarmStore`]: append, replay and item-grouped pagination.
//!
//! `(logical_id, seq)` is the primary key; a conflicting seq is reported as
//! [`StoreError::Conflict`] so the caller reloads `MAX(seq)` and retries the
//! whole run. `append_items` is one transaction: a failure leaves no row
//! behind, because a partial run would leave a hole in the log.

use super::budget::{classify, unexpected_row_count, unexplained, within_batch, within_budget};
use super::codec::{bind_item, bind_session_update, decode_item, seq_field, seq_param};
use super::sql::{
    INSERT_ITEM_SQL, SELECT_ITEMS_AFTER_SQL, SELECT_ITEMS_BEFORE_SQL, SELECT_ITEMS_TAIL_SQL,
    SELECT_ITEM_BY_ID_SQL, SELECT_ITEM_PAGE_ASC_SQL, SELECT_ITEM_PAGE_DESC_SQL,
    SELECT_ITEM_PAGE_TAIL_SQL, SELECT_LAST_SEQ_SQL, UPDATE_SESSION_SQL,
};
use super::{commit, rollback, SwarmStateStore};
use crate::domain::ItemOrder;
use crate::store::{
    grouped_item_page, StoreError, SwarmItemPage, SwarmItemRow, SwarmSessionUpdate,
};

impl SwarmStateStore {
    pub(super) async fn append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
        let affected = within_budget(
            "append_item",
            bind_item(sqlx::query(INSERT_ITEM_SQL), &row)?.execute(self.pool()),
        )
        .await?
        .rows_affected();
        match affected {
            1 => Ok(()),
            other => Err(unexplained("append_item", other)),
        }
    }

    pub(super) async fn append_items(
        &self,
        rows: &[SwarmItemRow],
        progress: Option<SwarmSessionUpdate>,
    ) -> Result<(), StoreError> {
        let statements = rows.len() + usize::from(progress.is_some());
        if statements == 0 {
            return Ok(());
        }
        // One transaction: a viewer never sees an item the session cursor has
        // not reached, and a failed run leaves no hole for the retry to skip.
        let transaction = within_batch("append_items", statements, async move {
            let mut transaction = self.begin("append_items").await?;
            for row in rows {
                let affected = bind_item(sqlx::query(INSERT_ITEM_SQL), row)?
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| classify("append_items", error))?
                    .rows_affected();
                if affected != 1 {
                    rollback("append_items", transaction).await;
                    return Err(unexplained("append_items", affected));
                }
            }
            if let Some(update) = &progress {
                // The progress update is the run's last statement; a zero here
                // means the session row disappeared mid-turn.
                let affected = bind_session_update(sqlx::query(UPDATE_SESSION_SQL), update)?
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| classify("append_items", error))?
                    .rows_affected();
                if affected != 1 {
                    let logical_id = update.logical_id.clone();
                    rollback("append_items", transaction).await;
                    return Err(StoreError::NotFound(format!(
                        "swarm session {logical_id} does not exist"
                    )));
                }
            }
            Ok(transaction)
        })
        .await?;
        commit("append_items", transaction).await
    }

    pub(super) async fn items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        let rows = within_budget(
            "items_after",
            sqlx::query(SELECT_ITEMS_AFTER_SQL)
                .bind(logical_id.to_owned())
                .bind(seq_param(after, "cursor")?)
                .bind(i64::from(limit))
                .fetch_all(self.pool()),
        )
        .await?;
        rows.iter().map(decode_item).collect()
    }

    pub(super) async fn items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        // Two static statements: `Some(before)` binds the cursor, the tail read
        // must not bind `u64::MAX` (it does not fit an i64).
        let rows = match before {
            Some(before) => {
                within_budget(
                    "items_before",
                    sqlx::query(SELECT_ITEMS_BEFORE_SQL)
                        .bind(logical_id.to_owned())
                        .bind(seq_param(before, "cursor")?)
                        .bind(i64::from(limit))
                        .fetch_all(self.pool()),
                )
                .await?
            }
            None => {
                within_budget(
                    "items_before",
                    sqlx::query(SELECT_ITEMS_TAIL_SQL)
                        .bind(logical_id.to_owned())
                        .bind(i64::from(limit))
                        .fetch_all(self.pool()),
                )
                .await?
            }
        };
        rows.iter().map(decode_item).collect()
    }

    pub(super) async fn item_page(
        &self,
        logical_id: &str,
        order: ItemOrder,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SwarmItemPage, StoreError> {
        // `limit + 1` items are selected so `has_more` needs no second query;
        // the extra item's rows are dropped by `grouped_item_page`.
        let probe = limit.saturating_add(1);
        let rows = match (order, after) {
            (ItemOrder::Asc, after) => {
                within_budget(
                    "item_page",
                    sqlx::query(SELECT_ITEM_PAGE_ASC_SQL)
                        .bind(logical_id.to_owned())
                        .bind(seq_param(after.unwrap_or(0), "cursor")?)
                        .bind(i64::from(probe))
                        .fetch_all(self.pool()),
                )
                .await?
            }
            (ItemOrder::Desc, Some(after)) => {
                within_budget(
                    "item_page",
                    sqlx::query(SELECT_ITEM_PAGE_DESC_SQL)
                        .bind(logical_id.to_owned())
                        .bind(seq_param(after, "cursor")?)
                        .bind(i64::from(probe))
                        .fetch_all(self.pool()),
                )
                .await?
            }
            (ItemOrder::Desc, None) => {
                within_budget(
                    "item_page",
                    sqlx::query(SELECT_ITEM_PAGE_TAIL_SQL)
                        .bind(logical_id.to_owned())
                        .bind(i64::from(probe))
                        .fetch_all(self.pool()),
                )
                .await?
            }
        };
        let rows = rows
            .iter()
            .map(decode_item)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(grouped_item_page(rows, order, limit))
    }

    pub(super) async fn last_seq(&self, logical_id: &str) -> Result<u64, StoreError> {
        let last: i64 = within_budget(
            "last_seq",
            sqlx::query_scalar(SELECT_LAST_SEQ_SQL)
                .bind(logical_id.to_owned())
                .fetch_one(self.pool()),
        )
        .await?;
        seq_field(last, "last item seq")
    }

    pub(super) async fn get_item(
        &self,
        logical_id: &str,
        item_id: &str,
    ) -> Result<Option<SwarmItemRow>, StoreError> {
        let rows = within_budget(
            "get_item",
            sqlx::query(SELECT_ITEM_BY_ID_SQL)
                .bind(logical_id.to_owned())
                .bind(item_id.to_owned())
                .fetch_all(self.pool()),
        )
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_item(row).map(Some),
            rows => Err(unexpected_row_count("get_item", rows.len())),
        }
    }
}
