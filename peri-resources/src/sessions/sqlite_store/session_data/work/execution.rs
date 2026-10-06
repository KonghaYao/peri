use super::*;
use crate::sessions::work_store::{SqlParam, SqlRows, SqlStatement};
use sqlx::sqlite::SqliteArguments;
use sqlx::{Arguments, Row, TypeInfo, ValueRef};

pub(super) fn arguments(parameters: &[SqlParam]) -> SessionResourceResult<SqliteArguments> {
    let mut arguments = SqliteArguments::default();
    for parameter in parameters {
        let result = match parameter {
            SqlParam::Text(value) => arguments.add(value.clone()),
            SqlParam::Integer(value) => arguments.add(*value),
            SqlParam::Null => arguments.add(Option::<String>::None),
            SqlParam::Blob(value) => arguments.add(value.clone()),
        };
        result.map_err(|error| {
            tracing::error!(%error, "SQLite work argument encoding failed");
            invalid_input("work SQL argument is not encodable")
        })?;
    }
    Ok(arguments)
}

pub(super) async fn read_rows(
    connection: &mut SqliteConnection,
    statement: &'static str,
    parameters: &[SqlParam],
) -> SessionResourceResult<SqlRows> {
    let rows = sqlx::query_with(statement, arguments(parameters)?)
        .fetch_all(connection)
        .await
        .map_err(sql_failure)?;
    rows.into_iter()
        .map(|row| {
            (0..row.len())
                .map(|index| {
                    let value = row.try_get_raw(index).map_err(sql_failure)?;
                    if value.is_null() {
                        return Ok(SqlParam::Null);
                    }
                    match value.type_info().name() {
                        "TEXT" => row.try_get(index).map(SqlParam::Text).map_err(sql_failure),
                        "INTEGER" | "BOOLEAN" => row.try_get(index).map(SqlParam::Integer).map_err(sql_failure),
                        "BLOB" => row.try_get(index).map(SqlParam::Blob).map_err(sql_failure),
                        kind => {
                            tracing::error!(%kind, index, "SQLite work column has unsupported type");
                            Err(corrupt("unsupported work SQL column type"))
                        }
                    }
                })
                .collect()
        })
        .collect()
}

pub(in crate::sessions::sqlite_store) async fn read_plan(
    connection: &mut SqliteConnection,
    statements: &[SqlStatement],
) -> SessionResourceResult<Vec<SqlRows>> {
    let mut rows = Vec::with_capacity(statements.len());
    for statement in statements {
        rows.push(read_rows(connection, statement.sql, &statement.params).await?);
    }
    Ok(rows)
}

pub(in crate::sessions::sqlite_store) async fn execute_plan(
    connection: &mut SqliteConnection,
    statements: &[SqlStatement],
) -> SessionResourceResult<()> {
    for statement in statements {
        execute_statement(
            connection,
            statement.sql,
            &statement.params,
            statement.expected_rows,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn execute_statement(
    connection: &mut SqliteConnection,
    statement: &'static str,
    parameters: &[SqlParam],
    expected_rows: Option<u64>,
) -> SessionResourceResult<()> {
    let result = sqlx::query_with(statement, arguments(parameters)?)
        .execute(connection)
        .await
        .map_err(sql_failure)?;
    if let Some(expected) = expected_rows {
        if result.rows_affected() != expected {
            tracing::warn!(
                expected,
                actual = result.rows_affected(),
                "SQLite work guard failed"
            );
            return Err(SessionResourceError::conflict(
                "work transaction guard failed",
            ));
        }
    }
    Ok(())
}

pub(in crate::sessions::sqlite_store) fn sql_failure(error: sqlx::Error) -> SessionResourceError {
    tracing::error!(%error, "SQLite work statement failed");
    map_sqlx(&error)
}

pub(super) async fn rollback(
    transaction: sqlx::Transaction<'_, sqlx::Sqlite>,
    failure: SessionResourceError,
    session_id: &str,
) -> SessionResourceError {
    tracing::warn!(%session_id, error = ?failure, "SQLite work transaction rejected; rolling back");
    if let Err(error) = transaction.rollback().await {
        tracing::error!(%error, %session_id, cause = ?failure, "SQLite work rollback failed");
        return commit_failure(Some(session_id.to_owned()));
    }
    failure
}

#[cfg(test)]
#[path = "execution_test.rs"]
mod tests;
