use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::oauth_credentials::{
    validate_credentials, validate_server_key, OAuthCredentialError, OAuthCredentialPort,
    OAuthCredentialResult,
};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::WorkspaceId;
use turso_serverless::Value;

use crate::sessions::canonical::{
    DELETE_ALL_V2_OAUTH_CREDENTIALS_SQL, DELETE_V2_OAUTH_CREDENTIAL_SQL,
    LIST_V2_OAUTH_CREDENTIALS_SQL, SELECT_V2_OAUTH_CREDENTIAL_SQL, UPSERT_V2_OAUTH_CREDENTIAL_SQL,
};

use super::ledger::{input_digest, OperationId, OperationIdentity};
use super::mutation::{MutationOutcome, QualifiedMutation};
use super::session_data::RemoteSessionData;
use super::sql::StatementSpec;

pub(super) struct RemoteOAuthCredentials(pub(super) Arc<RemoteSessionData>, pub(super) WorkspaceId);

fn scope(workspace_id: WorkspaceId) -> (&'static str, String) {
    ("local", workspace_id.to_string())
}

fn statement(
    sql: &'static str,
    principal: &str,
    workspace_id: &str,
    key: Option<&str>,
) -> StatementSpec {
    let mut params = vec![
        Value::Text(principal.into()),
        Value::Text(workspace_id.into()),
    ];
    if let Some(key) = key {
        params.push(Value::Text(key.into()));
    }
    StatementSpec::new(sql, params)
}

fn save_statement(principal: &str, workspace_id: &str, key: &str, payload: &str) -> StatementSpec {
    let mut spec = statement(
        UPSERT_V2_OAUTH_CREDENTIAL_SQL,
        principal,
        workspace_id,
        Some(key),
    );
    spec.params.push(Value::Text(payload.into()));
    spec.params.push(Value::Text(peri_time::now_utc_rfc3339()));
    spec
}

fn storage_error(error: SessionResourceError) -> OAuthCredentialError {
    match error.kind() {
        SessionResourceErrorKind::ReadOnlyStore => OAuthCredentialError::ReadOnly,
        SessionResourceErrorKind::Corrupt { .. } => OAuthCredentialError::InvalidData,
        _ => OAuthCredentialError::Unavailable,
    }
}

fn write_result(outcome: MutationOutcome) -> OAuthCredentialResult<()> {
    match outcome {
        MutationOutcome::Applied { .. } => Ok(()),
        MutationOutcome::NotApplied { class, .. } => {
            Err(storage_error(class.into_session_resource_error()))
        }
        MutationOutcome::ClosedNeverApplied | MutationOutcome::Unknown { .. } => {
            Err(OAuthCredentialError::Unavailable)
        }
    }
}

fn decode_load(rows: Vec<Vec<Value>>) -> OAuthCredentialResult<Option<String>> {
    match rows.as_slice() {
        [] => Ok(None),
        [row] => match row.as_slice() {
            [Value::Text(payload)] => {
                validate_credentials(payload).map_err(|_| OAuthCredentialError::InvalidData)?;
                Ok(Some(payload.clone()))
            }
            _ => Err(OAuthCredentialError::InvalidData),
        },
        _ => Err(OAuthCredentialError::InvalidData),
    }
}

fn decode_list(rows: Vec<Vec<Value>>) -> OAuthCredentialResult<Vec<String>> {
    rows.into_iter()
        .map(|row| match row.as_slice() {
            [Value::Text(key)] => {
                validate_server_key(key).map_err(|_| OAuthCredentialError::InvalidData)?;
                Ok(key.clone())
            }
            _ => Err(OAuthCredentialError::InvalidData),
        })
        .collect()
}

fn mutation(behavior: &str, spec: StatementSpec) -> QualifiedMutation {
    let inputs: Vec<&str> = spec
        .params
        .iter()
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let identity = OperationIdentity::with_digest(
        OperationId::mint(&ThreadId::from("oauth")),
        behavior,
        input_digest(&inputs),
    );
    QualifiedMutation {
        identity,
        effects: vec![spec],
    }
}

impl RemoteOAuthCredentials {
    async fn read(&self, spec: StatementSpec) -> OAuthCredentialResult<Vec<Vec<Value>>> {
        let store = self.0.store().await.map_err(storage_error)?;
        let mut results = store.read_batch(vec![spec]).await.map_err(storage_error)?;
        results.pop().ok_or(OAuthCredentialError::InvalidData)
    }

    async fn write(&self, behavior: &str, spec: StatementSpec) -> OAuthCredentialResult<()> {
        let mutation = mutation(behavior, spec);
        let store = self.0.store().await.map_err(storage_error)?;
        let (outcome, _) = store
            .apply_qualified_reporting(&mutation)
            .await
            .map_err(storage_error)?;
        write_result(outcome)
    }
}

#[async_trait]
impl OAuthCredentialPort for RemoteOAuthCredentials {
    async fn load(&self, server_key: &str) -> OAuthCredentialResult<Option<String>> {
        validate_server_key(server_key)?;
        let (principal, workspace_id) = scope(self.1);
        decode_load(
            self.read(statement(
                SELECT_V2_OAUTH_CREDENTIAL_SQL,
                principal,
                &workspace_id,
                Some(server_key),
            ))
            .await?,
        )
    }

    async fn save(&self, server_key: &str, credentials: &str) -> OAuthCredentialResult<()> {
        validate_server_key(server_key)?;
        validate_credentials(credentials)?;
        let (principal, workspace_id) = scope(self.1);
        self.write(
            "save_oauth_credentials",
            save_statement(principal, &workspace_id, server_key, credentials),
        )
        .await
    }

    async fn clear(&self, server_key: &str) -> OAuthCredentialResult<()> {
        validate_server_key(server_key)?;
        let (principal, workspace_id) = scope(self.1);
        self.write(
            "clear_oauth_credentials",
            statement(
                DELETE_V2_OAUTH_CREDENTIAL_SQL,
                principal,
                &workspace_id,
                Some(server_key),
            ),
        )
        .await
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        let (principal, workspace_id) = scope(self.1);
        self.write(
            "clear_all_oauth_credentials",
            statement(
                DELETE_ALL_V2_OAUTH_CREDENTIALS_SQL,
                principal,
                &workspace_id,
                None,
            ),
        )
        .await
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        let (principal, workspace_id) = scope(self.1);
        decode_list(
            self.read(statement(
                LIST_V2_OAUTH_CREDENTIALS_SQL,
                principal,
                &workspace_id,
                None,
            ))
            .await?,
        )
    }
}

#[cfg(test)]
#[path = "oauth_credentials_test.rs"]
mod tests;
