//! Remove obsolete execution ownership from remote schema 12 and 13 stores.

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use turso_serverless::Value;

use crate::sessions::canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL;

use super::{
    mutation::RemoteStore,
    schema::{self, StoreSnapshot},
    sql::StatementSpec,
};

pub(super) async fn upgrade(
    store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    if !matches!(snapshot.schema_version, 12 | 13) || snapshot.contract != schema::STORE_CONTRACT {
        return Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ));
    }
    store.apply_schema_upgrade(vec![
        StatementSpec::new(
            "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
                (SELECT 1 FROM peri_store_meta WHERE singleton = 0 AND schema_version = ?1 AND store_id = ?2 AND contract = ?3)",
            vec![Value::Integer(snapshot.schema_version), Value::Text(snapshot.store_id.as_str().to_owned()), Value::Text(snapshot.contract.clone())],
        ),
        StatementSpec::bare("DROP TABLE IF EXISTS session_execution_workspace_descriptors"),
        StatementSpec::bare("DROP TABLE IF EXISTS session_execution_owners"),
        StatementSpec::bare(CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL),
        StatementSpec::new(
            "UPDATE peri_store_meta SET schema_version = 14 WHERE singleton = 0 AND schema_version = ?1 AND store_id = ?2 AND contract = ?3",
            vec![Value::Integer(snapshot.schema_version), Value::Text(snapshot.store_id.as_str().to_owned()), Value::Text(snapshot.contract.clone())],
        ),
    ]).await
}
