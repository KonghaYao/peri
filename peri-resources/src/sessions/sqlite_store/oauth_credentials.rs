use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use peri_acp_types::oauth_credentials::{
    validate_credentials, validate_server_key, OAuthCredentialError, OAuthCredentialPort,
    OAuthCredentialResult,
};
use sqlx::AssertSqlSafe;

use super::database::SqliteSessionDatabase;

use crate::sessions::canonical;

pub(super) struct SqliteOAuthCredentialStore {
    database: Arc<SqliteSessionDatabase>,
    principal_id: String,
    machine_id: Option<String>,
}

impl SqliteOAuthCredentialStore {
    pub(super) fn new(database: Arc<SqliteSessionDatabase>) -> Self {
        Self {
            database,
            principal_id: "local".into(),
            machine_id: crate::sessions::machine::current().ok().map(str::to_owned),
        }
    }

    #[cfg(test)]
    fn with_scope(
        database: Arc<SqliteSessionDatabase>,
        principal_id: &str,
        machine_id: &str,
    ) -> Self {
        Self {
            database,
            principal_id: principal_id.into(),
            machine_id: Some(machine_id.into()),
        }
    }

    fn scope(&self, write: bool) -> OAuthCredentialResult<(&str, &str)> {
        if self.database.pool.is_closed() {
            return Err(OAuthCredentialError::Unavailable);
        }
        if write && self.database.is_read_only() {
            return Err(OAuthCredentialError::ReadOnly);
        }
        let machine_id = self
            .machine_id
            .as_deref()
            .ok_or(OAuthCredentialError::Unavailable)?;
        Ok((&self.principal_id, machine_id))
    }
}

#[async_trait]
impl OAuthCredentialPort for SqliteOAuthCredentialStore {
    async fn load(&self, server_key: &str) -> OAuthCredentialResult<Option<String>> {
        validate_server_key(server_key)?;
        let (principal_id, machine_id) = self.scope(false)?;
        let row: Option<(String,)> =
            sqlx::query_as(AssertSqlSafe(canonical::SELECT_OAUTH_CREDENTIAL_SQL))
                .bind(principal_id)
                .bind(machine_id)
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
        let (principal_id, machine_id) = self.scope(true)?;
        sqlx::query(AssertSqlSafe(canonical::UPSERT_OAUTH_CREDENTIAL_SQL))
            .bind(principal_id)
            .bind(machine_id)
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
        let (principal_id, machine_id) = self.scope(true)?;
        sqlx::query(AssertSqlSafe(canonical::DELETE_OAUTH_CREDENTIAL_SQL))
            .bind(principal_id)
            .bind(machine_id)
            .bind(server_key)
            .execute(&self.database.pool)
            .await
            .map_err(|_| OAuthCredentialError::Unavailable)?;
        Ok(())
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        let (principal_id, machine_id) = self.scope(true)?;
        sqlx::query(AssertSqlSafe(canonical::DELETE_ALL_OAUTH_CREDENTIALS_SQL))
            .bind(principal_id)
            .bind(machine_id)
            .execute(&self.database.pool)
            .await
            .map_err(|_| OAuthCredentialError::Unavailable)?;
        Ok(())
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        let (principal_id, machine_id) = self.scope(false)?;
        let rows: Vec<(String,)> =
            sqlx::query_as(AssertSqlSafe(canonical::LIST_OAUTH_CREDENTIALS_SQL))
                .bind(principal_id)
                .bind(machine_id)
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
