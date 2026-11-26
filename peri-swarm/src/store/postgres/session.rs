//! Session, listing and delete halves of [`SwarmStore`].
//!
//! Delete is one transaction (`design §4.4`): items, inputs, turns, the binding
//! hand-over, the remaining binding and the session row either all change or
//! none does. A partial delete reported as success would strand another
//! session's binding.

use super::budget::{classify, unexpected_row_count, unexplained, within_batch, within_budget};
use super::codec::{
    bind_binding, bind_session, bind_session_update, decode_session, patch_session_params,
};
use super::sql::{
    session_list_sql, ATTACH_BINDING_SQL, DELETE_BINDINGS_SQL, DELETE_INPUTS_SQL, DELETE_ITEMS_SQL,
    DELETE_SESSION_SQL, DELETE_TURNS_SQL, INSERT_SESSION_SQL, PATCH_SESSION_SQL,
    REASSIGN_BINDINGS_SQL, SELECT_SESSION_SQL, UPDATE_SESSION_SQL, UPSERT_BINDING_SQL,
    UPSERT_SESSION_SQL,
};
use super::{commit, rollback, SwarmStateStore};
use crate::domain::SessionsWindow;
use crate::store::{
    StoreError, SwarmBindingRow, SwarmSessionPatch, SwarmSessionRow, SwarmSessionUpdate,
};

impl SwarmStateStore {
    pub(super) async fn upsert_session(&self, row: SwarmSessionRow) -> Result<(), StoreError> {
        let affected = within_budget(
            "upsert_session",
            bind_session(sqlx::query(UPSERT_SESSION_SQL), &row)?.execute(self.pool()),
        )
        .await?
        .rows_affected();
        match affected {
            1 => Ok(()),
            0 => Err(StoreError::Conflict(
                "swarm session identity conflict: cwd or rootSessionId differs".to_owned(),
            )),
            other => Err(unexplained("upsert_session", other)),
        }
    }

    pub(super) async fn create_hosted_session(
        &self,
        row: SwarmSessionRow,
        binding: SwarmBindingRow,
        require_existing: bool,
    ) -> Result<(), StoreError> {
        let logical_env_id = binding.logical_env_id.clone();
        // The binding update and the session insert form one write transaction:
        // an identity conflict rolls back the binding, and a missing or replaced
        // shared environment leaves both statements without effect.
        within_batch("create_hosted_session", 2, async {
            let mut transaction = self.begin("create_hosted_session").await?;
            if require_existing {
                let attached = sqlx::query(ATTACH_BINDING_SQL)
                    .bind(binding.logical_env_id.clone())
                    .bind(binding.logical_id.clone())
                    .bind(binding.sandbox_id.clone())
                    .bind(binding.workspace_path.clone())
                    .bind(binding.last_run_id.clone())
                    .bind(binding.last_used_at.clone())
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| classify("create_hosted_session", error))?
                    .rows_affected();
                if attached != 1 {
                    rollback("create_hosted_session", transaction).await;
                    return Err(StoreError::NotFound(logical_env_id));
                }
            } else {
                let affected = bind_binding(sqlx::query(UPSERT_BINDING_SQL), &binding)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| classify("create_hosted_session", error))?
                    .rows_affected();
                if affected != 1 {
                    rollback("create_hosted_session", transaction).await;
                    return Err(unexplained("create_hosted_session", affected));
                }
            }
            let inserted = bind_session(sqlx::query(INSERT_SESSION_SQL), &row)?
                .execute(&mut *transaction)
                .await
                .map_err(|error| classify("create_hosted_session", error))?
                .rows_affected();
            if inserted != 1 {
                rollback("create_hosted_session", transaction).await;
                return Err(StoreError::NotFound(logical_env_id));
            }
            commit("create_hosted_session", transaction).await
        })
        .await
    }

    pub(super) async fn get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError> {
        let rows = within_budget(
            "get_session",
            sqlx::query(SELECT_SESSION_SQL)
                .bind(logical_id.to_owned())
                .fetch_all(self.pool()),
        )
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_session(row).map(Some),
            rows => Err(unexpected_row_count("get_session", rows.len())),
        }
    }

    pub(super) async fn update_session(
        &self,
        update: SwarmSessionUpdate,
    ) -> Result<(), StoreError> {
        let affected = within_budget(
            "update_session",
            bind_session_update(sqlx::query(UPDATE_SESSION_SQL), &update)?.execute(self.pool()),
        )
        .await?
        .rows_affected();
        match affected {
            1 => Ok(()),
            0 => Err(StoreError::NotFound(format!(
                "swarm session {} does not exist",
                update.logical_id
            ))),
            other => Err(unexplained("update_session", other)),
        }
    }

    pub(super) async fn patch_session(&self, patch: SwarmSessionPatch) -> Result<(), StoreError> {
        let logical_id = patch.logical_id.clone();
        let query = patch_session_params(&patch)?.bind(sqlx::query(PATCH_SESSION_SQL));
        let affected = within_budget("patch_session", query.execute(self.pool()))
            .await?
            .rows_affected();
        match affected {
            1 => Ok(()),
            0 => Err(StoreError::NotFound(format!(
                "swarm session {logical_id} does not exist"
            ))),
            other => Err(unexplained("patch_session", other)),
        }
    }

    pub(super) async fn list_sessions(
        &self,
        window: &SessionsWindow,
    ) -> Result<Vec<SwarmSessionRow>, StoreError> {
        if window
            .agent_id
            .as_deref()
            .is_some_and(|agent_id| agent_id != "root")
        {
            // The gateway has one root agent identity; a different id selects
            // no session rather than being ignored.
            return Ok(Vec::new());
        }
        // Only static fragments and numeric placeholders are concatenated; every
        // caller value travels as a bound parameter, so the assembled text is
        // safe to hand to SQLx verbatim.
        let sql = session_list_sql(window.order, window.metadata.len());
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(window.after.clone().unwrap_or_default())
            .bind(i64::from(window.limit.saturating_add(1)));
        for (key, value) in &window.metadata {
            query = query.bind(key.clone()).bind(value.clone());
        }
        let rows = within_budget("list_sessions", query.fetch_all(self.pool())).await?;
        rows.iter().map(decode_session).collect()
    }

    pub(super) async fn delete_session(&self, logical_id: &str) -> Result<(), StoreError> {
        // One transaction: a partial delete can never be reported as success.
        // Statements are ordered child-first for readability; the transaction is
        // what makes the set atomic.
        within_batch("delete_session", 6, async {
            let mut transaction = self.begin("delete_session").await?;
            for (operation, sql) in [
                ("delete_items", DELETE_ITEMS_SQL),
                ("delete_inputs", DELETE_INPUTS_SQL),
                ("delete_turns", DELETE_TURNS_SQL),
                ("reassign_bindings", REASSIGN_BINDINGS_SQL),
                ("delete_bindings", DELETE_BINDINGS_SQL),
                ("delete_session", DELETE_SESSION_SQL),
            ] {
                sqlx::query(sql)
                    .bind(logical_id.to_owned())
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| classify(operation, error))?;
            }
            commit("delete_session", transaction).await
        })
        .await
    }
}
