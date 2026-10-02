use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use peri_acp_types::oauth_credentials::{
    validate_credentials, validate_server_key, OAuthCredentialError, OAuthCredentialPort,
    OAuthCredentialResult,
};
use peri_acp_types::workspace::WorkspaceId;
use sqlx::AssertSqlSafe;

use super::database::SqliteSessionDatabase;

use crate::sessions::canonical;

pub(super) struct SqliteOAuthCredentialStore {
    database: Arc<SqliteSessionDatabase>,
    principal_id: String,
    workspace_id: String,
}

impl SqliteOAuthCredentialStore {
    pub(super) fn new(database: Arc<SqliteSessionDatabase>, workspace_id: WorkspaceId) -> Self {
        Self {
            database,
            principal_id: "local".into(),
            workspace_id: workspace_id.to_string(),
        }
    }

    #[cfg(test)]
    fn with_scope(
        database: Arc<SqliteSessionDatabase>,
        principal_id: &str,
        workspace_id: &str,
    ) -> Self {
        Self {
            database,
            principal_id: principal_id.into(),
            workspace_id: workspace_id.into(),
        }
    }

    fn scope(&self, write: bool) -> OAuthCredentialResult<(&str, &str)> {
        if self.database.pool.is_closed() {
            return Err(OAuthCredentialError::Unavailable);
        }
        if write && self.database.is_read_only() {
            return Err(OAuthCredentialError::ReadOnly);
        }
        Ok((&self.principal_id, &self.workspace_id))
    }
}

#[async_trait]
impl OAuthCredentialPort for SqliteOAuthCredentialStore {
    async fn load(&self, server_key: &str) -> OAuthCredentialResult<Option<String>> {
        validate_server_key(server_key)?;
        let (principal_id, workspace_id) = self.scope(false)?;
        let row: Option<(String,)> =
            sqlx::query_as(AssertSqlSafe(canonical::SELECT_V2_OAUTH_CREDENTIAL_SQL))
                .bind(principal_id)
                .bind(workspace_id)
                .bind(server_key)
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|_| OAuthCredentialError::Unavailable)?;
        match row {
            Some((credentials,)) => {
                validate_credentials(&credentials)
                    .map_err(|_| OAuthCredentialError::InvalidData)?;
                Ok(Some(credentials))
            }
            None => Ok(None),
        }
    }

    async fn save(&self, server_key: &str, credentials: &str) -> OAuthCredentialResult<()> {
        validate_server_key(server_key)?;
        validate_credentials(credentials)?;
        let (principal_id, workspace_id) = self.scope(true)?;
        sqlx::query(AssertSqlSafe(canonical::UPSERT_V2_OAUTH_CREDENTIAL_SQL))
            .bind(principal_id)
            .bind(workspace_id)
            .bind(server_key)
            .bind(credentials)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.database.pool)
            .await
            .map_err(|_| OAuthCredentialError::Unavailable)?;
        Ok(())
    }

    async fn clear(&self, server_key: &str) -> OAuthCredentialResult<()> {
        validate_server_key(server_key)?;
        let (principal_id, workspace_id) = self.scope(true)?;
        sqlx::query(AssertSqlSafe(canonical::DELETE_V2_OAUTH_CREDENTIAL_SQL))
            .bind(principal_id)
            .bind(workspace_id)
            .bind(server_key)
            .execute(&self.database.pool)
            .await
            .map_err(|_| OAuthCredentialError::Unavailable)?;
        Ok(())
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        let (principal_id, workspace_id) = self.scope(true)?;
        sqlx::query(AssertSqlSafe(
            canonical::DELETE_ALL_V2_OAUTH_CREDENTIALS_SQL,
        ))
        .bind(principal_id)
        .bind(workspace_id)
        .execute(&self.database.pool)
        .await
        .map_err(|_| OAuthCredentialError::Unavailable)?;
        Ok(())
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        let (principal_id, workspace_id) = self.scope(false)?;
        let rows: Vec<(String,)> =
            sqlx::query_as(AssertSqlSafe(canonical::LIST_V2_OAUTH_CREDENTIALS_SQL))
                .bind(principal_id)
                .bind(workspace_id)
                .fetch_all(&self.database.pool)
                .await
                .map_err(|_| OAuthCredentialError::Unavailable)?;
        rows.into_iter()
            .map(|(server_key,)| {
                validate_server_key(&server_key).map_err(|_| OAuthCredentialError::InvalidData)?;
                Ok(server_key)
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "oauth_credentials_test.rs"]
mod tests;
