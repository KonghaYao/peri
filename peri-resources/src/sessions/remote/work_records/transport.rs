use peri_acp_types::session_resources::SessionResourceResult;
use turso_serverless::Value;

use super::super::{session_data::invalid_input, sql::StatementSpec};
use crate::sessions::{
    failure::corrupt,
    work_store::{SqlParam, SqlRows, SqlStatement},
};

const MAX_STATEMENTS: usize = 1024;
const MAX_PARAMETERS: usize = 128;
const MAX_TRANSACTION_BYTES: usize = 32 * 1024 * 1024;

pub(in crate::sessions::remote) fn specifications(
    statements: Vec<SqlStatement>,
) -> SessionResourceResult<Vec<StatementSpec>> {
    let mut converted = Vec::with_capacity(statements.len());
    for statement in statements {
        converted.push(StatementSpec::new(
            statement.sql,
            statement.params.into_iter().map(value).collect(),
        ));
        if let Some(expected) = statement.expected_rows {
            converted.push(StatementSpec::new(
                "INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE changes()<>?1",
                vec![Value::Integer(i64::try_from(expected).map_err(|_| {
                    corrupt("affected row count exceeds SQLite range")
                })?)],
            ));
        }
    }
    validate_budget(&converted)?;
    Ok(converted)
}

pub(super) fn value(parameter: SqlParam) -> Value {
    match parameter {
        SqlParam::Text(text) => Value::Text(text),
        SqlParam::Integer(integer) => Value::Integer(integer),
        SqlParam::Null => Value::Null,
        SqlParam::Blob(bytes) => Value::Blob(bytes),
    }
}

pub(in crate::sessions::remote) fn rows(
    batches: Vec<Vec<Vec<Value>>>,
) -> SessionResourceResult<Vec<SqlRows>> {
    batches
        .into_iter()
        .map(|batch| {
            batch
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|column| match column {
                            Value::Text(text) => Ok(SqlParam::Text(text)),
                            Value::Integer(integer) => Ok(SqlParam::Integer(integer)),
                            Value::Null => Ok(SqlParam::Null),
                            Value::Blob(bytes) => Ok(SqlParam::Blob(bytes)),
                            _ => Err(corrupt("remote work row has an unsupported parameter type")),
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

pub(in crate::sessions::remote) fn validate_budget(
    statements: &[StatementSpec],
) -> SessionResourceResult<()> {
    if statements.len().saturating_add(1) > MAX_STATEMENTS {
        return Err(invalid_input(
            "remote work transaction exceeds statement budget",
        ));
    }
    let mut total_bytes = 4096usize;
    for statement in statements {
        if statement.params.len() > MAX_PARAMETERS {
            return Err(invalid_input(
                "remote work statement exceeds parameter budget",
            ));
        }
        total_bytes = total_bytes.saturating_add(statement.sql.len());
        for parameter in &statement.params {
            let bytes = match parameter {
                Value::Text(text) => text.len().saturating_mul(6),
                Value::Blob(bytes) => bytes.len().saturating_mul(2),
                Value::Integer(_) | Value::Null => 32,
                _ => return Err(invalid_input("unsupported remote work parameter type")),
            };
            total_bytes = total_bytes.saturating_add(bytes).saturating_add(64);
        }
        if total_bytes > MAX_TRANSACTION_BYTES {
            return Err(invalid_input("remote work transaction exceeds byte budget"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod tests;
