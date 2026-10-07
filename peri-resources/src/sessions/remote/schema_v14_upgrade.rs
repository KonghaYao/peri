//! Guarded remote schema 12–17 upgrade removing retired execution recovery.

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
    if !matches!(snapshot.schema_version, 12..=17) || snapshot.contract != schema::STORE_CONTRACT {
        return Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ));
    }
    let results = store
        .read_batch(vec![StatementSpec::bare(
            crate::sessions::schema_cleanup::SCHEMA_OBJECTS_SQL,
        )])
        .await?;
    let objects = super::schema_upgrade::decode_objects(&results[0])?;
    let removals = crate::sessions::schema_cleanup::execution_recovery_removal_plan(&objects)
        .map_err(|_| SessionResourceError::new(SessionResourceErrorKind::Unsupported))?;
    let mut plan = vec![StatementSpec::new(
        super::schema_upgrade::GUARD_SNAPSHOT_SQL,
        vec![
            Value::Integer(objects.len() as i64),
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    )];
    for object in &objects {
        plan.push(StatementSpec::new(
            super::schema_upgrade::GUARD_OBJECT_SQL,
            vec![
                Value::Text(object.kind.clone()),
                Value::Text(object.name.clone()),
                Value::Text(object.table.clone()),
                object.sql.clone().map(Value::Text).unwrap_or(Value::Null),
            ],
        ));
    }
    if snapshot.schema_version != 17 {
        plan.extend([
            StatementSpec::bare("DROP TABLE IF EXISTS session_execution_workspace_descriptors"),
            StatementSpec::bare("DROP TABLE IF EXISTS session_execution_owners"),
            StatementSpec::bare(CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL),
        ]);
    }
    plan.extend(removals.iter().map(|sql| StatementSpec::bare(sql)));
    plan.push(StatementSpec::new(
        "UPDATE peri_store_meta SET schema_version = 18 WHERE singleton = 0 AND schema_version = ?1 AND store_id = ?2 AND contract = ?3",
        vec![Value::Integer(snapshot.schema_version), Value::Text(snapshot.store_id.as_str().to_owned()), Value::Text(snapshot.contract.clone())],
    ));
    store.apply_schema_upgrade(plan).await
}
