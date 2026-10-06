use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use turso_serverless::Value;

use super::mutation::RemoteStore;
use super::schema::{self, StoreSnapshot};
use super::sql::{int_at, text_at, StatementSpec};
use crate::sessions::canonical;
use crate::sessions::schema_cleanup::{self, ColumnShape, SchemaObject};

const GUARD_SNAPSHOT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM sqlite_master WHERE substr(lower(name), 1, 7) <> 'sqlite_') <> ?1
    OR NOT EXISTS (SELECT 1 FROM peri_store_meta WHERE singleton = 0 AND schema_version = ?2 AND store_id = ?3 AND contract = ?4)";
const GUARD_OBJECT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
    (SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2 AND tbl_name = ?3 AND sql IS ?4)";
const ADVANCE_VERSION_SQL: &str = "UPDATE peri_store_meta SET schema_version = ?1 WHERE singleton = 0 AND schema_version = ?2 AND store_id = ?3 AND contract = ?4";

pub(super) async fn upgrade(
    store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    if matches!(snapshot.schema_version, 12..=16) && snapshot.contract == schema::STORE_CONTRACT {
        return super::schema_v14_upgrade::upgrade(store, snapshot).await;
    }
    if snapshot.schema_version == 11 && snapshot.contract == "peri.session.store/v2" {
        super::schema_v12_upgrade::upgrade(store, snapshot).await?;
        let super::schema::StoreIdentityRead::Present(upgraded) = store.read_identity().await?
        else {
            return Err(invalid_schema());
        };
        return super::schema_v14_upgrade::upgrade(store, &upgraded).await;
    }
    let results = store
        .read_batch(vec![
            StatementSpec::bare(schema_cleanup::SCHEMA_OBJECTS_SQL),
            StatementSpec::bare(schema_cleanup::THREAD_COLUMNS_SQL),
            StatementSpec::bare(schema_cleanup::MESSAGE_COLUMNS_SQL),
        ])
        .await?;
    let mut objects = Vec::new();
    for row in &results[0] {
        objects.push(SchemaObject {
            kind: text_at(row, 0).ok_or_else(invalid_schema)?.to_owned(),
            name: text_at(row, 1).ok_or_else(invalid_schema)?.to_owned(),
            table: text_at(row, 2).ok_or_else(invalid_schema)?.to_owned(),
            sql: optional_string(row, 3)?,
        });
    }
    let columns = decode_columns(&results[1])?;
    let messages = decode_columns(&results[2])?;
    if !columns.is_empty()
        && !canonical::MESSAGE_COLUMN_NAMES.iter().all(|required| {
            messages
                .iter()
                .any(|column| column.name.eq_ignore_ascii_case(required))
        })
    {
        return Err(invalid_schema());
    }
    store
        .apply_schema_upgrade(upgrade_plan(snapshot, &objects, &columns)?)
        .await?;
    let super::schema::StoreIdentityRead::Present(upgraded) = store.read_identity().await? else {
        return Err(invalid_schema());
    };
    super::schema_v12_upgrade::upgrade(store, &upgraded).await?;
    let super::schema::StoreIdentityRead::Present(upgraded) = store.read_identity().await? else {
        return Err(invalid_schema());
    };
    super::schema_v14_upgrade::upgrade(store, &upgraded).await
}

fn decode_columns(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<ColumnShape>> {
    let mut columns = Vec::new();
    for row in rows {
        columns.push(ColumnShape {
            name: text_at(row, 0).ok_or_else(invalid_schema)?.to_owned(),
            kind: text_at(row, 1).ok_or_else(invalid_schema)?.to_owned(),
            not_null: int_at(row, 2).ok_or_else(invalid_schema)?,
            default: optional_string(row, 3)?,
            primary_key: int_at(row, 4).ok_or_else(invalid_schema)?,
            hidden: int_at(row, 5).ok_or_else(invalid_schema)?,
        });
    }
    Ok(columns)
}

pub(super) fn upgrade_plan(
    snapshot: &StoreSnapshot,
    objects: &[SchemaObject],
    columns: &[ColumnShape],
) -> SessionResourceResult<Vec<StatementSpec>> {
    if snapshot.schema_version != 10 || snapshot.contract != "peri.session.store/v2" {
        return Err(invalid_schema());
    }
    if !objects.iter().any(|object| {
        object.name == schema::STORE_META_TABLE
            && object.kind == "table"
            && object.sql.as_deref().is_some_and(|sql| {
                schema_cleanup::known_definition(sql, schema::CREATE_STORE_META_SQL)
            })
    }) {
        return Err(invalid_schema());
    }
    let empty = columns.is_empty();
    if empty {
        if objects.iter().any(|object| {
            [
                "threads",
                "messages",
                "projects",
                "workspaces",
                "session_bindings",
                "thread_goals",
            ]
            .iter()
            .any(|name| object.name.eq_ignore_ascii_case(name))
        }) {
            return Err(invalid_schema());
        }
    } else if !canonical::THREAD_COLUMN_NAMES.iter().all(|required| {
        columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(required))
    }) || !objects
        .iter()
        .any(|object| object.kind == "table" && object.name.eq_ignore_ascii_case("messages"))
    {
        return Err(invalid_schema());
    }
    let removals = schema_cleanup::removal_plan(objects, columns).map_err(|_| invalid_schema())?;
    let mut plan = vec![StatementSpec::new(
        GUARD_SNAPSHOT_SQL,
        vec![
            Value::Integer(objects.len() as i64),
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    )];
    for object in objects {
        plan.push(StatementSpec::new(
            GUARD_OBJECT_SQL,
            vec![
                Value::Text(object.kind.clone()),
                Value::Text(object.name.clone()),
                Value::Text(object.table.clone()),
                object.sql.clone().map(Value::Text).unwrap_or(Value::Null),
            ],
        ));
    }
    plan.extend(removals.into_iter().map(StatementSpec::bare));
    if empty {
        plan.extend(
            canonical::CREATE_TABLES
                .iter()
                .chain(canonical::CREATE_INDEXES)
                .map(|sql| StatementSpec::bare(sql)),
        );
    }
    plan.push(StatementSpec::new(
        ADVANCE_VERSION_SQL,
        vec![
            Value::Integer(11),
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    ));
    Ok(plan)
}

fn optional_string(row: &[Value], index: usize) -> SessionResourceResult<Option<String>> {
    match row.get(index) {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(value)) => Ok(Some(value.clone())),
        _ => Err(invalid_schema()),
    }
}

fn invalid_schema() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}
