use crate::sessions::schema_cleanup::{self, ColumnShape, SchemaObject};
use anyhow::Result;
use peri_acp_types::workspace::WorkspaceError;
use sqlx::SqliteConnection;

pub(super) async fn removal_plan(connection: &mut SqliteConnection) -> Result<Vec<&'static str>> {
    let rows: Vec<(String, String, String, Option<String>)> =
        sqlx::query_as(schema_cleanup::SCHEMA_OBJECTS_SQL)
            .fetch_all(&mut *connection)
            .await?;
    let objects = rows
        .into_iter()
        .map(|(kind, name, table, sql)| SchemaObject {
            kind,
            name,
            table,
            sql,
        })
        .collect::<Vec<_>>();
    let rows: Vec<(String, String, i64, Option<String>, i64, i64)> =
        sqlx::query_as(schema_cleanup::THREAD_COLUMNS_SQL)
            .fetch_all(&mut *connection)
            .await?;
    let columns = rows
        .into_iter()
        .map(
            |(name, kind, not_null, default, primary_key, hidden)| ColumnShape {
                name,
                kind,
                not_null,
                default,
                primary_key,
                hidden,
            },
        )
        .collect::<Vec<_>>();
    schema_cleanup::removal_plan(&objects, &columns)
        .map_err(|_| WorkspaceError::UnsupportedDatabaseSchema.into())
}
