//! Add the shared durable execution generation to a schema 12 remote store.

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use turso_serverless::Value;

use super::{
    mutation::RemoteStore,
    schema::{self, StoreSnapshot},
    sql::StatementSpec,
};
use crate::sessions::canonical;

pub(super) async fn upgrade(
    store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    if snapshot.schema_version != 12 || snapshot.contract != schema::STORE_CONTRACT {
        return Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ));
    }
    store.apply_schema_upgrade(vec![
        StatementSpec::bare(canonical::CREATE_SESSION_EXECUTION_OWNERS_TABLE_SQL),
        StatementSpec::bare(canonical::CREATE_SESSION_EXECUTION_WORKSPACE_DESCRIPTORS_TABLE_SQL),
        StatementSpec::new(
            "UPDATE peri_store_meta SET schema_version = 13 WHERE singleton = 0 AND schema_version = 12 AND store_id = ?1 AND contract = ?2",
            vec![Value::Text(snapshot.store_id.as_str().to_owned()), Value::Text(schema::STORE_CONTRACT.to_owned())],
        ),
    ]).await
}
