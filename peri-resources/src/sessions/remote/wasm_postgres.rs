//! PostgreSQL over the Neon SQL HTTP protocol for hosts without TCP sockets.
//! A whole transaction is one fetch. The proxy returns PostgreSQL text values,
//! so 64-bit counters never pass through a JavaScript number.
use serde::Deserialize;
use serde_json::{json, Value as Json};

use super::failure::{sqlstate_class, RemoteFailureClass};
use super::sql::{StatementSpec, Value};
use super::{TransportError, TransportResult, LOCK_STORE};

const PIN_TRANSACTION_SCHEMA: &str =
    "SELECT pg_catalog.set_config('search_path', pg_catalog.quote_ident(pg_catalog.current_schema()), true)";

fn request_body(statements: &[StatementSpec], writable: bool) -> Json {
    let mut queries = vec![json!({ "query": PIN_TRANSACTION_SCHEMA, "params": [] })];
    if writable {
        queries.push(json!({ "query": LOCK_STORE, "params": [] }));
    }
    queries.extend(statements.iter().map(|spec| {
        let params: Vec<Option<String>> = spec.params.iter().map(parameter).collect();
        json!({ "query": spec.sql, "params": params })
    }));
    json!({ "queries": queries })
}

fn parameter(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text.clone()),
        Value::Integer(n) => Some(n.to_string()),
        Value::Real(n) => Some(n.to_string()),
        Value::Blob(bytes) => {
            use std::fmt::Write;
            let mut hex = String::from("\\x");
            for byte in bytes {
                write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
            }
            Some(hex)
        }
    }
}

#[derive(Deserialize)]
struct QueryResult {
    fields: Vec<Field>,
    rows: Vec<Vec<Option<String>>>,
    // The proxy sends null for utility commands (e.g. CREATE INDEX).
    #[serde(rename = "rowCount")]
    affected: Option<u64>,
    command: String,
}

#[derive(Deserialize)]
struct Field {
    #[serde(rename = "dataTypeID")]
    oid: u32,
}

fn malformed() -> TransportError {
    TransportError::Uncertain(RemoteFailureClass::Corrupt)
}

fn results(body: &str, count: usize, writable: bool) -> TransportResult<Vec<QueryResult>> {
    #[derive(Deserialize)]
    struct Batch {
        results: Vec<QueryResult>,
    }
    let batch: Batch = serde_json::from_str(body).map_err(|_| malformed())?;
    let prefix = if writable { 2 } else { 1 };
    if batch.results.len() != count + prefix {
        return Err(malformed());
    }
    Ok(batch.results.into_iter().skip(prefix).collect())
}

impl QueryResult {
    fn affected_rows(self) -> TransportResult<u64> {
        match (self.affected, self.command.as_str()) {
            (Some(count), _) => Ok(count),
            (None, "CREATE" | "ALTER" | "DROP" | "SET" | "LOCK" | "DO") => Ok(0),
            _ => Err(malformed()),
        }
    }

    fn values(self) -> TransportResult<Vec<Vec<Value>>> {
        self.rows
            .into_iter()
            .map(|row| {
                if row.len() != self.fields.len() {
                    return Err(malformed());
                }
                row.into_iter()
                    .zip(&self.fields)
                    .map(|(cell, field)| match cell {
                        None => Ok(Value::Null),
                        Some(raw) => decode(field.oid, raw).ok_or_else(malformed),
                    })
                    .collect()
            })
            .collect()
    }
}

fn decode(oid: u32, raw: String) -> Option<Value> {
    match oid {
        16 => match raw.as_str() {
            "t" => Some(Value::Integer(1)),
            "f" => Some(Value::Integer(0)),
            _ => None,
        },
        20 | 21 | 23 | 26 => raw.parse().ok().map(Value::Integer),
        700 | 701 => raw.parse().ok().map(Value::Real),
        17 => {
            let hex = raw.strip_prefix("\\x")?;
            if !hex.is_ascii() || !hex.len().is_multiple_of(2) {
                return None;
            }
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
                .collect::<Option<Vec<_>>>()
                .map(Value::Blob)
        }
        18 | 19 | 25 | 1042 | 1043 => Some(Value::Text(raw)),
        _ => None,
    }
}

fn server_failure(status: u16, body: &str, statements: &[StatementSpec]) -> TransportError {
    #[derive(Deserialize)]
    struct PgError {
        code: String,
        table: Option<String>,
        constraint: Option<String>,
    }
    if status == 401 || status == 403 {
        return TransportError::Before(RemoteFailureClass::AuthRejected);
    }
    if status != 400 {
        return TransportError::Uncertain(RemoteFailureClass::Transport);
    }
    let Ok(error) = serde_json::from_str::<PgError>(body) else {
        return TransportError::Uncertain(RemoteFailureClass::Transport);
    };
    let class = sqlstate_class(&error.code);
    // A connection failure can race COMMIT. Propagate uncertainty, even if a proxy
    // supplied a PostgreSQL SQLSTATE in an HTTP 400 response.
    if error.code.starts_with("08") || error.code == "40003" || error.code.starts_with("57P0") {
        return TransportError::Uncertain(class);
    }
    if error.code.len() != 5
        || !error
            .code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return TransportError::Uncertain(RemoteFailureClass::Transport);
    }
    // Neon supplies no statement index. An identified primary key is sufficient only
    // if a single statement in this batch can have inserted into that table.
    let unique = error.code == "23505";
    let target = match (error.table.as_deref(), error.constraint.as_deref()) {
        (Some("peri_op_ledger"), Some("peri_op_ledger_pkey")) => Some("peri_op_ledger"),
        (Some("peri_store_meta"), Some("peri_store_meta_pkey")) => Some("peri_store_meta"),
        _ => None,
    };
    let index = target.and_then(|table| {
        let mut candidates = statements.iter().enumerate().filter(|(_, s)| {
            s.sql.strip_prefix("INSERT INTO ").is_some_and(|rest| {
                rest.split(|c: char| c.is_ascii_whitespace() || c == '(')
                    .next()
                    == Some(table)
            })
        });
        let (index, _) = candidates.next()?;
        candidates.next().is_none().then_some(index)
    });
    TransportError::RolledBack {
        class,
        index,
        unique,
    }
}

/// Automatic routing is safe only for Neon's own domain. Other endpoints must
/// be named explicitly by the host because the proxy receives the database DSN.
fn neon_endpoint(dsn: &url::Url) -> Option<String> {
    let host = dsn.domain()?;
    let (_, suffix) = host.split_once('.')?;
    if suffix != "neon.tech" && !suffix.ends_with(".neon.tech") {
        return None;
    }
    Some(format!("https://api.{suffix}/sql"))
}

#[cfg(target_os = "emscripten")]
pub(super) use host::connect;

#[cfg(target_os = "emscripten")]
mod host {
    use async_trait::async_trait;
    use js_sys::Promise;
    use peri_acp_types::session_resources::SessionResourceResult;
    use std::sync::Arc;
    use tokio::sync::oneshot;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{spawn_local, JsFuture};

    use super::super::{RemoteEndpoint, RemoteTransport, SessionStoreCredential};
    use super::*;

    #[wasm_bindgen(inline_js = r#"
export function periPostgresRequest(endpoint, dsn, body, writable) {
    const controller = new AbortController();
    const response = (async () => {
        const r = await globalThis.fetch(endpoint, {
            method: 'POST', redirect: 'error', credentials: 'omit',
            signal: controller.signal,
            headers: {
                'Content-Type': 'application/json',
                'Neon-Connection-String': dsn,
                'Neon-Raw-Text-Output': 'true',
                'Neon-Array-Mode': 'true',
                'Neon-Batch-Isolation-Level': writable ? 'ReadCommitted' : 'RepeatableRead',
                'Neon-Batch-Read-Only': writable ? 'false' : 'true',
            },
            body,
        });
        return [r.status, await r.text()];
    })();
    return {controller, response};
}
export function periPostgresResponse(handle) { return handle.response; }
export function periPostgresAbort(handle) { handle.controller.abort(); }
"#)]
    extern "C" {
        #[wasm_bindgen(catch, js_name = periPostgresRequest)]
        fn start(endpoint: &str, dsn: &str, body: &str, writable: bool)
            -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_name = periPostgresResponse)]
        fn response(handle: &JsValue) -> Promise;
        #[wasm_bindgen(js_name = periPostgresAbort)]
        fn abort(handle: &JsValue);
    }

    struct Http {
        endpoint: String,
        dsn: String,
    }

    pub(in crate::sessions::remote::connection) async fn connect(
        endpoint: &RemoteEndpoint,
        credential: &SessionStoreCredential,
    ) -> SessionResourceResult<Arc<dyn RemoteTransport>> {
        let invalid = || RemoteFailureClass::Unsupported.into_session_resource_error();
        let mut dsn = url::Url::parse(endpoint.sdk_url()).map_err(|_| invalid())?;
        if let Some(password) = credential.expose() {
            dsn.set_password(Some(password)).map_err(|_| invalid())?;
        }
        let endpoint = endpoint
            .http_endpoint()
            .map(str::to_owned)
            .or_else(|| neon_endpoint(&dsn))
            .ok_or_else(invalid)?;
        Ok(Arc::new(Http {
            endpoint,
            dsn: dsn.to_string(),
        }))
    }

    impl Http {
        async fn execute(
            &self,
            statements: &[StatementSpec],
            writable: bool,
        ) -> TransportResult<Vec<QueryResult>> {
            let (mut deliver, receive) = oneshot::channel();
            let endpoint = self.endpoint.clone();
            let dsn = self.dsn.clone();
            let body = request_body(statements, writable).to_string();
            // JS handles stay on the local executor. Dropping the caller (including
            // on timeout) cancels the fetch and does not retry the transaction.
            spawn_local(async move {
                let Ok(handle) = start(&endpoint, &dsn, &body, writable) else {
                    return;
                };
                let result = tokio::select! {
                    response = JsFuture::from(response(&handle)) => response.ok(),
                    _ = deliver.closed() => { abort(&handle); return; }
                };
                let decoded = result.and_then(|result| {
                    let parts = js_sys::Array::from(&result);
                    Some((parts.get(0).as_f64()? as u16, parts.get(1).as_string()?))
                });
                let _ = deliver.send(decoded);
            });
            let (status, body) = receive
                .await
                .ok()
                .flatten()
                .ok_or(TransportError::Uncertain(RemoteFailureClass::Transport))?;
            if !(200..300).contains(&status) {
                return Err(server_failure(status, &body, statements));
            }
            results(&body, statements.len(), writable)
        }
    }

    #[async_trait]
    impl RemoteTransport for Http {
        async fn sql_values(&self, spec: &StatementSpec) -> TransportResult<Vec<Vec<Value>>> {
            self.execute(std::slice::from_ref(spec), false)
                .await?
                .pop()
                .ok_or_else(malformed)?
                .values()
        }

        async fn managed_batch(&self, statements: Vec<StatementSpec>) -> TransportResult<Vec<u64>> {
            self.execute(&statements, true)
                .await?
                .into_iter()
                .map(QueryResult::affected_rows)
                .collect()
        }

        async fn consistent_read(
            &self,
            statements: Vec<StatementSpec>,
        ) -> TransportResult<Vec<Vec<Vec<Value>>>> {
            self.execute(&statements, false)
                .await?
                .into_iter()
                .map(QueryResult::values)
                .collect()
        }

        async fn close(&self) -> TransportResult<()> {
            // This transport owns no persistent database connection.
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "wasm_postgres_test.rs"]
mod tests;
