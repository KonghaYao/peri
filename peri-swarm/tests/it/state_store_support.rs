//! Shared helpers for the two-database PostgreSQL deployment tests.
//!
//! Split from the tests themselves to keep every file under `STD-SIZE-001`:
//! the deterministic model stub, the two DSNs, the workspace fixture and the
//! read-only schema probes used by both `state_store_test` and
//! `state_store_failure_test`.

//! Swarm deployment with two PostgreSQL databases: the Peri session store and
//! the independent Swarm state store.
//!
//! Set `PERI_SWARM_SESSION_STORE` and `PERI_SWARM_STATE_STORE` to PostgreSQL
//! DSNs that name **different databases** (the same instance is fine). No Loom
//! service or credential is involved; the model endpoint is a deterministic
//! stub. Without the two variables every test prints one skip line and returns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use parking_lot::Mutex;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::SessionStoreShutdownPort;
use peri_acp_types::store::PersistedPayload;
use peri_resources::{RemoteWorkspaceEnvironment, Resources};
use peri_swarm::actor::runtime::SessionStoreDeployment;
use peri_swarm::bootstrap::SESSION_STORE_ENV;
use peri_swarm::config::PostgresLocator;
use peri_swarm::domain::{McpHeader, McpToolSpec};
use peri_swarm::store::{SwarmStateStoreConfig, SWARM_STATE_SCHEMA_VERSION};

/// The **state** database DSN. The session store has its own variable.
pub(crate) const STATE_LOCATOR_ENV: &str = "PERI_SWARM_STATE_STORE";
/// Stable machine identity for this test deployment (`design §4.1`).
pub(crate) const MACHINE_ID: &str = "0192f0c4-0000-7000-8000-00000000bb02";

pub(crate) const TURN_PREFIX: &str = "stub-";

// ── model stub (same seam as `actor_runtime_test.rs`, simplified) ───────────

pub(crate) struct StubModel {
    calls: AtomicUsize,
    main_calls: AtomicUsize,
    bodies: Mutex<Vec<Value>>,
}

impl StubModel {
    pub(crate) fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            main_calls: AtomicUsize::new(0),
            bodies: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn main_calls(&self) -> usize {
        self.main_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

pub(crate) fn message_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        other => other.to_string(),
    }
}

pub(crate) async fn start_stub() -> (String, Arc<StubModel>) {
    let stub = Arc::new(StubModel::new());
    let router = Router::new()
        .fallback(any(stub_chat))
        .with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}/v1"), stub)
}

pub(crate) async fn stub_chat(State(stub): State<Arc<StubModel>>, body: Bytes) -> Response {
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    stub.calls.fetch_add(1, Ordering::SeqCst);
    if is_background_helper(&request) {
        // Peri's prediction/title helper runs beside the turn; it must not
        // consume the deterministic turn text or shift the request count.
        return sse_response(&[format!("{TURN_PREFIX}helper")], 0, "helper");
    }
    let index = stub.main_calls.fetch_add(1, Ordering::SeqCst);
    stub.bodies.lock().push(request);
    let text = format!("{TURN_PREFIX}{index}");
    sse_response(&[text], index, &format!("main-{index}"))
}

pub(crate) fn is_background_helper(request: &Value) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("content").is_some_and(|content| {
                    message_content_text(content).contains("<prediction_directive>")
                })
            })
        })
}

pub(crate) fn sse_response(texts: &[String], index: usize, tag: &str) -> Response {
    let mut sse = String::new();
    for text in texts {
        let (head, tail) = text.split_at(text.len().min(5));
        let frames = [
            json!({
                "id": format!("chatcmpl-stub-{tag}-{index}"),
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": { "role": "assistant", "content": head } }],
            }),
            json!({
                "id": format!("chatcmpl-stub-{tag}-{index}"),
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": { "content": tail } }],
            }),
            json!({
                "id": format!("chatcmpl-stub-{tag}-{index}"),
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            }),
        ];
        for frame in frames {
            sse.push_str("data: ");
            sse.push_str(&frame.to_string());
            sse.push_str("\n\n");
        }
    }
    sse.push_str("data: [DONE]\n\n");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        sse,
    )
        .into_response()
}

pub(crate) fn settings_json(base_url: &str) -> String {
    json!({
        "config": {
            "active_alias": "sonnet",
            "providers": [{
                "id": "local-stub",
                "type": "openai",
                "apiKey": "test-key-not-a-credential",
                "baseUrl": base_url,
            }],
            "profiles": {
                "sonnet": { "provider": "local-stub", "model": "stub-model" }
            }
        }
    })
    .to_string()
}

/// Keep the run hermetic: the ACP host enables Langfuse whenever the deployment
/// sees both keys, so an inherited developer environment would make shutdown
/// depend on a real endpoint.
pub(crate) fn clear_telemetry_env() {
    for key in [
        "LANGFUSE_PUBLIC_KEY",
        "LANGFUSE_SECRET_KEY",
        "LANGFUSE_HOST",
        "LANGFUSE_BASE_URL",
        "LANGFUSE_USER_ID",
        "LANGFUSE_TRACE_SAMPLING",
    ] {
        std::env::remove_var(key);
    }
}

// ── environment ─────────────────────────────────────────────────────────────

/// Both DSNs, or `None` when the test environment is not configured.
pub(crate) fn required_env() -> Option<(String, String)> {
    let session = std::env::var(SESSION_STORE_ENV).ok();
    let state = std::env::var(STATE_LOCATOR_ENV).ok();
    match (session, state) {
        (Some(session), Some(state)) if !session.trim().is_empty() && !state.trim().is_empty() => {
            Some((session, state))
        }
        _ => {
            eprintln!(
                "skipping the PostgreSQL state store deployment test: export \
                 {SESSION_STORE_ENV} and {STATE_LOCATOR_ENV} (PostgreSQL DSNs of two databases)"
            );
            None
        }
    }
}

pub(crate) fn deployment(session_store: &str) -> SessionStoreDeployment {
    SessionStoreDeployment::from_locator(session_store).with_engine("postgres")
}

pub(crate) fn state_store_config(state_store: &str) -> SwarmStateStoreConfig {
    SwarmStateStoreConfig {
        locator: PostgresLocator::parse(state_store, STATE_LOCATOR_ENV)
            .expect("the state store DSN is a PostgreSQL DSN"),
    }
}

/// The two DSNs must name different databases (bootstrap rejects a shared one).
pub(crate) fn assert_distinct_locators(session_store: &str, state_store: &str) {
    let session = PostgresLocator::parse(session_store, SESSION_STORE_ENV).expect("session DSN");
    let state = PostgresLocator::parse(state_store, STATE_LOCATOR_ENV).expect("state DSN");
    assert!(
        !state.same_database(&session),
        "the deployment needs two databases: the session store and the Swarm state store"
    );
}

pub(crate) fn machine_id() -> uuid::Uuid {
    uuid::Uuid::parse_str(MACHINE_ID).expect("machine id")
}

/// Canonical `(root, cwd)` under a temp directory: the virtual workspace rule
/// requires an absolute normalized root, and the host canonicalizes `cwd`, so
/// both sides of `contains` must agree byte-for-byte.
pub(crate) fn workspace() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().expect("workspace root");
    let root = std::fs::canonicalize(temp.path()).expect("canonical workspace root");
    let cwd = root.join("agent-state-cwd");
    std::fs::create_dir_all(&cwd).expect("workspace cwd");
    let cwd = std::fs::canonicalize(&cwd).expect("canonical cwd");
    (temp, root, cwd)
}

pub(crate) fn environment(machine_id: &uuid::Uuid, root: &Path) -> RemoteWorkspaceEnvironment {
    RemoteWorkspaceEnvironment::virtual_workspace(&machine_id.to_string(), root.to_path_buf())
        .expect("virtual workspace identity")
}

pub(crate) async fn open(
    store: &SessionStoreDeployment,
    env: &RemoteWorkspaceEnvironment,
) -> Resources {
    Resources::open_deployment_in_remote_environment(store, env.clone())
        .await
        .expect("open the Peri PostgreSQL session store")
}

/// Consume the deployment shutdown owner: drop the business handle, then close.
pub(crate) async fn close(resources: Resources) {
    let (session, shutdown) = resources.into_parts();
    drop(session);
    shutdown
        .shutdown()
        .await
        .expect("close the remote session store");
}

// ── PostgreSQL schema probes ────────────────────────────────────────────────

/// Tables the Swarm state database owns (`peri-swarm/src/store/postgres/schema.rs`).
pub(crate) const SWARM_TABLES: [&str; 6] = [
    "swarm_bindings",
    "swarm_inputs",
    "swarm_items",
    "swarm_meta",
    "swarm_sessions",
    "swarm_turns",
];

/// PostgreSQL session contract version, stored in `peri_store_meta`.
pub(crate) const PERI_SCHEMA_VERSION: i64 = 1;

pub(crate) fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(crate) async fn connect(dsn: &str) -> PgPool {
    PgPool::connect(dsn).await.expect("probe connection")
}

pub(crate) async fn current_schema(pool: &PgPool) -> String {
    sqlx::query_scalar::<_, String>("SELECT current_schema()::text")
        .fetch_one(pool)
        .await
        .expect("current_schema")
}

pub(crate) async fn relations(pool: &PgPool, schema: &str) -> Vec<(String, String)> {
    let rows = sqlx::query(
        "SELECT c.relkind::text, c.relname::text FROM pg_catalog.pg_class c
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = $1 ORDER BY c.relkind, c.relname",
    )
    .bind(schema.to_owned())
    .fetch_all(pool)
    .await
    .expect("catalog relations");
    rows.iter()
        .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
        .collect()
}

/// `(relkind, relname)` sorted, the isolation check's unit.
pub(crate) async fn schema_snapshot(pool: &PgPool) -> Vec<(String, String)> {
    let schema = current_schema(pool).await;
    relations(pool, &schema).await
}

#[derive(Debug, PartialEq)]
pub(crate) struct PeriSchema {
    pub(crate) objects: Vec<(String, String)>,
    pub(crate) meta: Vec<(i64, String, String)>,
}

pub(crate) async fn peri_schema(pool: &PgPool) -> PeriSchema {
    let schema = current_schema(pool).await;
    let objects = relations(pool, &schema).await;
    let meta = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT schema_version, store_id, contract FROM {}.peri_store_meta WHERE singleton = 0",
        quote(&schema)
    )))
    .fetch_all(pool)
    .await
    .expect("PostgreSQL schema metadata");
    let meta = meta
        .iter()
        .map(|row| {
            (
                row.get::<i64, _>(0),
                row.get::<String, _>(1),
                row.get::<String, _>(2),
            )
        })
        .collect();
    PeriSchema { objects, meta }
}

/// The state database owns exactly the Swarm schema, at the version this build
/// writes, and carries no Peri canonical object.
pub(crate) async fn assert_swarm_schema(pool: &PgPool) -> Vec<(String, String)> {
    let schema = current_schema(pool).await;
    let meta = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT version, engine, contract FROM {}.swarm_meta",
        quote(&schema)
    )))
    .fetch_all(pool)
    .await
    .expect("swarm_meta rows");
    assert_eq!(meta.len(), 1, "swarm_meta holds exactly one identity row");
    assert_eq!(meta[0].get::<i64, _>(0), SWARM_STATE_SCHEMA_VERSION);
    assert_eq!(meta[0].get::<String, _>(1), "postgres");
    assert_eq!(meta[0].get::<String, _>(2), "peri.swarm.postgres/v1");

    let mut tables: Vec<String> = relations(pool, &schema)
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "r" || kind == "p")
        .map(|(_, name)| name)
        .collect();
    tables.sort();
    let expected: Vec<String> = SWARM_TABLES.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(
        tables, expected,
        "the state database owns exactly the five Swarm tables plus swarm_meta"
    );

    let indexes: Vec<String> = relations(pool, &schema)
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "i" || kind == "I")
        .map(|(_, name)| name)
        .collect();
    for index in ["swarm_items_by_id", "swarm_turns_by_created"] {
        assert!(
            indexes.iter().any(|present| present == index),
            "missing index {index}: {indexes:?}"
        );
    }

    let foreign: Vec<String> = relations(pool, &schema)
        .await
        .into_iter()
        .map(|(_, name)| name)
        .filter(|name| name.starts_with("peri") || name == "threads" || name == "messages")
        .collect();
    assert!(
        foreign.is_empty(),
        "the state database must not adopt Peri canonical objects: {foreign:?}"
    );
    schema_snapshot(pool).await
}

/// Canonical MCP declarations for the round trip: `mcp_json` is the only place
/// the store keeps them, and the value is a test marker, not a credential.
pub(crate) fn declared_mcp_tools() -> Vec<McpToolSpec> {
    let mut tool = McpToolSpec::new("state-store", "https://mcp.example.invalid/state");
    tool.headers.push(McpHeader {
        name: "x-state-run".to_owned(),
        value: "not-a-credential".to_owned(),
    });
    tool.require_approval = Some(json!("never"));
    tool.allowed_tools = Some(json!(["lookup"]));
    vec![tool]
}

pub(crate) fn now() -> String {
    peri_time::now_utc_rfc3339()
}

pub(crate) async fn wait_until<F, Fut>(label: &str, within: Duration, mut probe: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if probe().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{label} did not become observable within {within:?}");
}

/// `(role, text)` of every persisted message the history read returns.
pub(crate) fn history_texts(history: &[PersistedPayload]) -> Vec<(String, String)> {
    history
        .iter()
        .filter_map(|payload| payload.as_message())
        .map(|message| match message {
            BaseMessage::Human { content, .. } => ("human".to_owned(), content.text_content()),
            BaseMessage::Ai { content, .. } => ("ai".to_owned(), content.text_content()),
            BaseMessage::System { content, .. } => ("system".to_owned(), content.text_content()),
            BaseMessage::Tool { content, .. } => ("tool".to_owned(), content.text_content()),
        })
        .collect()
}
