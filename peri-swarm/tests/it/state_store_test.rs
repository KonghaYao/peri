//! Swarm deployment with two PostgreSQL databases: the Peri session store and
//! the independent Swarm state store.
//!
//! Set `PERI_SWARM_SESSION_STORE` and `PERI_SWARM_STATE_STORE` to PostgreSQL
//! DSNs that name **different databases** (the same instance is fine). No Loom
//! service or credential is involved; the model endpoint is a deterministic
//! stub. Without the two variables every test prints one skip line and returns.

use std::collections::BTreeMap;
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
use tokio::sync::broadcast;

use peri_acp_types::messages::{BaseMessage, MessageContent, MessageId};
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageSource, MqEnqueueDisposition, MqFilter, MqInput,
};
use peri_acp_types::session_resources::SessionStoreShutdownPort;
use peri_acp_types::store::PersistedPayload;
use peri_resources::{RemoteWorkspaceEnvironment, Resources};
use peri_swarm::actor::reader::TurnOutcome;
use peri_swarm::actor::runtime::{AcpChild, ChildConfig, ChildError, SessionStoreDeployment};
use peri_swarm::actor::ItemLog;
use peri_swarm::bootstrap::{BootstrapConfig, MACHINE_ID_ENV, SESSION_STORE_ENV, STATE_STORE_ENV};
use peri_swarm::config::{ConfigError, PostgresLocator, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemSource, LogicalId, McpHeader,
    McpToolSpec, SessionState,
};
use peri_swarm::store::{
    InputPut, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow, SwarmStateError,
    SwarmStateStore, SwarmStateStoreConfig, SwarmStore, SWARM_STATE_SCHEMA_VERSION,
};

/// The **state** database DSN. The session store has its own variable.
const STATE_LOCATOR_ENV: &str = "PERI_SWARM_STATE_STORE";
/// Stable machine identity for this test deployment (`design §4.1`).
const MACHINE_ID: &str = "0192f0c4-0000-7000-8000-00000000bb02";

const TURN_PREFIX: &str = "stub-";

// ── model stub (same seam as `actor_runtime_test.rs`, simplified) ───────────

struct StubModel {
    calls: AtomicUsize,
    main_calls: AtomicUsize,
    bodies: Mutex<Vec<Value>>,
}

impl StubModel {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            main_calls: AtomicUsize::new(0),
            bodies: Mutex::new(Vec::new()),
        }
    }

    fn main_calls(&self) -> usize {
        self.main_calls.load(Ordering::SeqCst)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn message_content_text(content: &Value) -> String {
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

async fn start_stub() -> (String, Arc<StubModel>) {
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

async fn stub_chat(State(stub): State<Arc<StubModel>>, body: Bytes) -> Response {
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

fn is_background_helper(request: &Value) -> bool {
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

fn sse_response(texts: &[String], index: usize, tag: &str) -> Response {
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

fn settings_json(base_url: &str) -> String {
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
fn clear_telemetry_env() {
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
fn required_env() -> Option<(String, String)> {
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

fn deployment(session_store: &str) -> SessionStoreDeployment {
    SessionStoreDeployment::from_locator(session_store).with_engine("postgres")
}

fn state_store_config(state_store: &str) -> SwarmStateStoreConfig {
    SwarmStateStoreConfig {
        locator: PostgresLocator::parse(state_store, STATE_LOCATOR_ENV)
            .expect("the state store DSN is a PostgreSQL DSN"),
    }
}

/// The two DSNs must name different databases (bootstrap rejects a shared one).
fn assert_distinct_locators(session_store: &str, state_store: &str) {
    let session = PostgresLocator::parse(session_store, SESSION_STORE_ENV).expect("session DSN");
    let state = PostgresLocator::parse(state_store, STATE_LOCATOR_ENV).expect("state DSN");
    assert!(
        !state.same_database(&session),
        "the deployment needs two databases: the session store and the Swarm state store"
    );
}

fn machine_id() -> uuid::Uuid {
    uuid::Uuid::parse_str(MACHINE_ID).expect("machine id")
}

/// Canonical `(root, cwd)` under a temp directory: the virtual workspace rule
/// requires an absolute normalized root, and the host canonicalizes `cwd`, so
/// both sides of `contains` must agree byte-for-byte.
fn workspace() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().expect("workspace root");
    let root = std::fs::canonicalize(temp.path()).expect("canonical workspace root");
    let cwd = root.join("agent-state-cwd");
    std::fs::create_dir_all(&cwd).expect("workspace cwd");
    let cwd = std::fs::canonicalize(&cwd).expect("canonical cwd");
    (temp, root, cwd)
}

fn environment(machine_id: &uuid::Uuid, root: &Path) -> RemoteWorkspaceEnvironment {
    RemoteWorkspaceEnvironment::virtual_workspace(&machine_id.to_string(), root.to_path_buf())
        .expect("virtual workspace identity")
}

async fn open(store: &SessionStoreDeployment, env: &RemoteWorkspaceEnvironment) -> Resources {
    Resources::open_deployment_in_remote_environment(store, env.clone())
        .await
        .expect("open the Peri PostgreSQL session store")
}

/// Consume the deployment shutdown owner: drop the business handle, then close.
async fn close(resources: Resources) {
    let (session, shutdown) = resources.into_parts();
    drop(session);
    shutdown
        .shutdown()
        .await
        .expect("close the remote session store");
}

// ── PostgreSQL schema probes ────────────────────────────────────────────────

/// Tables the Swarm state database owns (`peri-swarm/src/store/postgres/schema.rs`).
const SWARM_TABLES: [&str; 6] = [
    "swarm_bindings",
    "swarm_inputs",
    "swarm_items",
    "swarm_meta",
    "swarm_sessions",
    "swarm_turns",
];

/// PostgreSQL session contract version, stored in `peri_store_meta`.
const PERI_SCHEMA_VERSION: i64 = 1;

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn connect(dsn: &str) -> PgPool {
    PgPool::connect(dsn).await.expect("probe connection")
}

async fn current_schema(pool: &PgPool) -> String {
    sqlx::query_scalar::<_, String>("SELECT current_schema()::text")
        .fetch_one(pool)
        .await
        .expect("current_schema")
}

async fn relations(pool: &PgPool, schema: &str) -> Vec<(String, String)> {
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
async fn schema_snapshot(pool: &PgPool) -> Vec<(String, String)> {
    let schema = current_schema(pool).await;
    relations(pool, &schema).await
}

#[derive(Debug, PartialEq)]
struct PeriSchema {
    objects: Vec<(String, String)>,
    meta: Vec<(i64, String, String)>,
}

async fn peri_schema(pool: &PgPool) -> PeriSchema {
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
async fn assert_swarm_schema(pool: &PgPool) -> Vec<(String, String)> {
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
fn declared_mcp_tools() -> Vec<McpToolSpec> {
    let mut tool = McpToolSpec::new("state-store", "https://mcp.example.invalid/state");
    tool.headers.push(McpHeader {
        name: "x-state-run".to_owned(),
        value: "not-a-credential".to_owned(),
    });
    tool.require_approval = Some(json!("never"));
    tool.allowed_tools = Some(json!(["lookup"]));
    vec![tool]
}

fn now() -> String {
    peri_time::now_utc_rfc3339()
}

async fn wait_until<F, Fut>(label: &str, within: Duration, mut probe: F) -> bool
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
fn history_texts(history: &[PersistedPayload]) -> Vec<(String, String)> {
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

// ── 1. two PostgreSQL databases ─────────────────────────────────────────────

/// `a` + `b` + `c` + `e`: the Peri session store carries the session/history/MQ
/// writes of a real turn, the independent Swarm state store carries the
/// session/item/input/binding rows, and both stay readable after the child and
/// every process connection are closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires two PostgreSQL databases"]
async fn postgres_carries_peri_session_history_mq_and_separate_swarm_state() {
    if std::env::var_os("SWARM_TRACE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    clear_telemetry_env();
    let Some((session_store_dsn, state_store_dsn)) = required_env() else {
        return;
    };
    assert_distinct_locators(&session_store_dsn, &state_store_dsn);

    let (base_url, stub) = start_stub().await;
    let (_workspace_temp, root, cwd) = workspace();
    let machine = machine_id();
    let env = environment(&machine, &root);
    let store = deployment(&session_store_dsn);

    // Process-level deployment: Peri reads and MQ writes go to the session
    // database; Swarm rows go to the independent state database with its own
    // `swarm_meta` schema.
    let resources = open(&store, &env).await;

    // ── schema, before the state database exists in this process ────────────
    // The session database carries the Peri schema only, at the version this
    // build writes.
    let peri_probe = connect(&session_store_dsn).await;
    let peri_before = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_before.meta.len(),
        1,
        "peri_store_meta holds exactly one row: {:?}",
        peri_before.meta
    );
    assert_eq!(
        peri_before.meta[0].0, PERI_SCHEMA_VERSION,
        "the session database is at the Peri schema version: {:?}",
        peri_before.meta
    );
    assert!(
        peri_before
            .objects
            .iter()
            .all(|(_, name)| !name.starts_with("swarm_")),
        "the session database must carry no swarm table or index: {:?}",
        peri_before.objects
    );
    eprintln!(
        "state store: peri schema_version={} contract={} schema_objects={}",
        peri_before.meta[0].0,
        peri_before.meta[0].2,
        peri_before.objects.len()
    );

    // The state store opens a second, independent database. Its first open in a
    // fresh database creates the owned schema and pins `swarm_meta`.
    let state_store = Arc::new(
        SwarmStateStore::open(&state_store_config(&state_store_dsn))
            .await
            .expect("open the independent swarm state store"),
    );
    let swarm_probe = connect(&state_store_dsn).await;
    let swarm_schema = assert_swarm_schema(&swarm_probe).await;
    eprintln!(
        "state store: swarm schema_version={} objects={:?}",
        SWARM_STATE_SCHEMA_VERSION,
        swarm_schema
            .iter()
            .map(|(kind, name)| format!("{kind}:{name}"))
            .collect::<Vec<_>>()
    );

    let store_rows = Arc::clone(&state_store);
    let run = uuid::Uuid::now_v7().to_string();
    eprintln!("state store run id: {run}");
    let logical = LogicalId::parse(&format!("agent-state-child-{run}")).expect("logical id");
    let root_session_id = uuid::Uuid::now_v7().to_string();
    let created_at = now();
    let declared_mcp = declared_mcp_tools();
    let declared_mcp_json = McpToolSpec::encode_declaration(&declared_mcp)
        .expect("encode the canonical MCP declaration")
        .expect("a non-empty declaration has canonical JSON");

    // The child's item log writes into the **state** database: S4 persists one
    // row per text delta with a stable `itemId`, and the turn terminal adds a
    // complete row with the same id. `swarm_items` allows that repeated id.
    let item_store: Arc<dyn SwarmStore> = store_rows.clone();

    // The Swarm session row for the Peri root session: the same identity
    // `AcpSwarmService` writes before its log starts.
    store_rows
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: logical.as_str().to_owned(),
            root_session_id: root_session_id.clone(),
            actor_key: format!("agent:{}", logical.as_str()),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Idle,
            model: Some("stub-model".to_owned()),
            instructions: Some("state store run".to_owned()),
            mcp_tools: declared_mcp.clone(),
            metadata: Default::default(),
            prompt_cache_options: None,
            environment: EnvironmentRef::none(),
            created_at: created_at.clone(),
            last_activity_at: created_at.clone(),
            last_item_seq: 0,
        })
        .await
        .expect("seed swarm session row");

    let log = Arc::new(ItemLog::new(
        logical.as_str(),
        item_store,
        broadcast::channel(256).0,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
    ));

    // ── a. real Peri host on the remote store, one turn ─────────────────────
    // Open the Swarm turn exactly like `AcpSwarmService::run_turn`: the log owns
    // `running`, the delta rows and the terminal.
    let request_id = "state-turn-1";
    log.begin_turn(request_id, "turn_state-1")
        .await
        .expect("open the swarm turn on the remote store");
    let child = AcpChild::spawn(
        ChildConfig {
            cwd: cwd.clone(),
            session_store: store.clone(),
            machine_id: Some(machine),
            workspace_root: Some(root.clone()),
            settings_json: Some(settings_json(&base_url)),
            model: None,
            instructions: Some("state store run".to_owned()),
            mcp_tools: Vec::new(),
            workspace: None,
            root_session_id: root_session_id.clone(),
        },
        Arc::clone(&log),
    )
    .await
    .expect("spawn the Peri host on the PostgreSQL session store");
    assert_eq!(
        child.session_id(),
        root_session_id,
        "the reserved root session id is the ACP session id"
    );

    let reply = child
        .prompt("hello postgres state", request_id)
        .await
        .expect("run one turn");
    // The production service closes the turn on the reader's terminal, not on
    // the prompt reply. Waiting for that terminal also drains items the reader
    // may still hold in its coalescing window, so the completion row is built
    // from the whole stream; the direct close is only the safety net.
    if !log
        .wait_turn_closed(request_id, Duration::from_secs(30))
        .await
    {
        log.finish_turn(
            request_id,
            TurnOutcome::Completed(
                reply
                    .stop_reason
                    .clone()
                    .expect("the host reports a terminal stop reason"),
            ),
        )
        .await;
    }
    assert_eq!(
        reply.stop_reason.as_deref(),
        Some("end_turn"),
        "turn reply: {reply:?}"
    );
    assert_eq!(
        stub.main_calls(),
        1,
        "one turn request (total requests incl. helpers: {})",
        stub.calls()
    );

    // The turn is projected into `swarm_items` through the real state store:
    // every delta and the terminal complete row reuse **one** item id, each at
    // its own strictly increasing seq. Both are checked here (state + row
    // shape), not assumed.
    let child_session_live = store_rows
        .swarm_get_session(logical.as_str())
        .await
        .expect("child swarm session after the turn")
        .expect("the child swarm session row exists");
    assert_eq!(
        child_session_live.state,
        SessionState::Idle,
        "a persisted turn ends idle; `failed` would mean an append conflict"
    );
    let child_items = store_rows
        .swarm_items_after(logical.as_str(), 0, 100)
        .await
        .expect("child swarm items after the turn");
    let seqs: Vec<u64> = child_items.iter().map(|row| row.seq).collect();
    assert!(
        seqs.len() >= 2,
        "the streamed answer must persist several rows: {child_items:?}"
    );
    assert_eq!(seqs.first(), Some(&1), "seq starts at 1: {seqs:?}");
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "seq is strictly increasing: {seqs:?}"
    );
    let deltas: Vec<_> = child_items
        .iter()
        .filter(|row| row.payload["delta"].as_bool() == Some(true))
        .collect();
    let completes: Vec<_> = child_items
        .iter()
        .filter(|row| row.payload["complete"].as_bool() == Some(true))
        .collect();
    assert!(
        deltas.len() >= 2,
        "at least two delta rows share the stream id: {child_items:?}"
    );
    assert_eq!(
        completes.len(),
        1,
        "the turn terminal appends exactly one complete row: {child_items:?}"
    );
    let stream_id = completes[0].item_id.as_str();
    assert!(
        child_items.iter().all(|row| row.item_id == stream_id),
        "every delta/complete row reuses the stable item id {stream_id}: {child_items:?}"
    );
    assert_eq!(
        completes[0].payload["text"].as_str(),
        Some(format!("{TURN_PREFIX}0").as_str()),
        "the complete row carries the whole accumulated text"
    );
    assert_eq!(
        child_session_live.last_item_seq,
        *seqs.last().expect("at least one row"),
        "the cached projection equals MAX(seq)"
    );

    // Independent SQL over the state database: the same turn rows exist there.
    let schema = current_schema(&swarm_probe).await;
    let item_rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT seq, item_id, payload_json FROM {}.swarm_items WHERE logical_id = $1 ORDER BY seq",
        quote(&schema)
    )))
    .bind(logical.as_str())
    .fetch_all(&swarm_probe)
    .await
    .expect("swarm_items rows");
    assert_eq!(
        item_rows.len(),
        child_items.len(),
        "the database holds exactly the rows the store read"
    );
    for row in &item_rows {
        assert_eq!(
            row.get::<String, _>(1),
            stream_id,
            "every persisted row reuses item id {stream_id}"
        );
    }
    let payloads: Vec<String> = item_rows
        .iter()
        .map(|row| row.get::<String, _>(2))
        .collect();
    assert_eq!(
        payloads
            .iter()
            .filter(|payload| payload.contains("\"complete\":true"))
            .count(),
        1,
        "the complete row is persisted in the state database: {payloads:?}"
    );
    assert!(
        payloads
            .iter()
            .filter(|payload| payload.contains("\"delta\":true"))
            .count()
            >= 2,
        "delta rows are persisted in the state database: {payloads:?}"
    );
    eprintln!(
        "state store: swarm item rows={} seqs={:?} item_id={stream_id}",
        item_rows.len(),
        item_rows
            .iter()
            .map(|row| row.get::<i64, _>(0))
            .collect::<Vec<_>>()
    );

    // `mcp_json` is the canonical declaration text, not a derived shape.
    let mcp_rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT mcp_json FROM {}.swarm_sessions WHERE logical_id = $1",
        quote(&schema)
    )))
    .bind(logical.as_str())
    .fetch_all(&swarm_probe)
    .await
    .expect("mcp_json rows");
    assert_eq!(
        mcp_rows,
        vec![declared_mcp_json.clone()],
        "the state database persists the canonical MCP declaration verbatim"
    );
    assert_eq!(
        child_session_live.mcp_tools, declared_mcp,
        "the child session row reads its declaration back"
    );

    // Peri session + history, read through the session database.
    let reader = resources.session_resources();
    let session_id = root_session_id.clone();
    wait_until("Peri session history", Duration::from_secs(30), || {
        let reader = Arc::clone(&reader);
        let session_id = session_id.clone();
        async move {
            match reader.load_session_history(&session_id).await {
                Ok(history) => history_texts(&history)
                    .iter()
                    .any(|(role, text)| role == "human" && text.contains("hello postgres state")),
                Err(_) => false,
            }
        }
    })
    .await;

    let meta = reader
        .load_session_meta(&root_session_id)
        .await
        .expect("Peri session metadata from PostgreSQL");
    assert_eq!(meta.id, root_session_id);
    assert_eq!(meta.cwd, cwd.to_string_lossy());
    let history = reader
        .load_session_history(&root_session_id)
        .await
        .expect("Peri history from PostgreSQL");
    let texts = history_texts(&history);
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "human" && text.contains("hello postgres state")),
        "the user turn is persisted: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "ai" && text.contains(&format!("{TURN_PREFIX}0"))),
        "the assistant answer is persisted: {texts:?}"
    );
    assert!(
        meta.message_count >= 2,
        "session metadata counts the persisted messages: {}",
        meta.message_count
    );

    // Peri MQ: a durable row through the same remote store.
    let mq_id = MessageId::new();
    let mq_text = "state store durable mq row";
    let outcome = reader
        .mq_enqueue(
            &root_session_id,
            MqInput {
                id: mq_id,
                kind: MessageKind::Prompt,
                source: MessageSource::UserInput,
                payload: PersistedPayload::Message(BaseMessage::Human {
                    id: mq_id,
                    content: MessageContent::text(mq_text),
                }),
                policy: MessagePolicy::ensure_processing(),
                original_draft: Some(mq_text.to_owned()),
                plugin_selections: Vec::new(),
            },
        )
        .await
        .expect("MQ write through the remote store");
    assert_eq!(outcome.disposition, MqEnqueueDisposition::Inserted);
    assert_eq!(
        reader.mq_count(&root_session_id).await.expect("MQ count"),
        1,
        "one durable MQ row before close"
    );

    // ── b. Swarm rows through the process-level state store ─────────────────
    let rows_logical = LogicalId::parse(&format!("agent-state-rows-{run}")).expect("logical id");
    let binding_env_id = format!("logical-env-state-{run}");
    let rows_root_session = uuid::Uuid::now_v7().to_string();
    store_rows
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: rows_logical.as_str().to_owned(),
            root_session_id: rows_root_session.clone(),
            actor_key: format!("agent:{}", rows_logical.as_str()),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Running,
            model: Some("stub-model".to_owned()),
            instructions: Some("state swarm rows".to_owned()),
            mcp_tools: declared_mcp.clone(),
            metadata: Default::default(),
            prompt_cache_options: None,
            environment: EnvironmentRef {
                kind: EnvironmentKind::None,
                id: Some(rows_logical.as_str().to_owned()),
            },
            created_at: created_at.clone(),
            last_activity_at: created_at.clone(),
            last_item_seq: 0,
        })
        .await
        .expect("write swarm session row");

    for seq in 1..=2u64 {
        store_rows
            .swarm_append_item(SwarmItemRow {
                logical_id: rows_logical.as_str().to_owned(),
                seq,
                item_id: format!("itim_state_{run}_{seq}"),
                kind: ItemKind::Message,
                source: ItemSource::root(),
                payload: json!({
                    "state": true,
                    "seq": seq,
                    "text": format!("swarm row {seq}"),
                    "complete": true,
                }),
                created_at: now(),
                truncated: false,
            })
            .await
            .expect("append swarm item row");
    }

    let input_row = SwarmInputRow {
        logical_id: rows_logical.as_str().to_owned(),
        input_id: format!("state-input-{run}"),
        fingerprint: "fp-state-1".to_owned(),
        state: InputState::Accepted,
        created_at: now(),
    };
    assert_eq!(
        store_rows
            .swarm_put_input(input_row.clone())
            .await
            .expect("write swarm input row"),
        InputPut::Inserted
    );

    let binding_row = SwarmBindingRow {
        logical_env_id: binding_env_id.clone(),
        logical_id: rows_logical.as_str().to_owned(),
        sandbox_id: "sandbox-state".to_owned(),
        workspace_path: cwd.to_string_lossy().into_owned(),
        sandbox_root: root.to_string_lossy().into_owned(),
        settings_profile: Some("sonnet".to_owned()),
        token_file: None,
        last_run_id: Some("run-state".to_owned()),
        last_used_at: now(),
    };
    store_rows
        .swarm_put_binding(binding_row.clone())
        .await
        .expect("write swarm binding row");

    let rows_last_seq = store_rows
        .swarm_last_seq(rows_logical.as_str())
        .await
        .expect("last seq before close");
    assert_eq!(rows_last_seq, 2);
    // `swarm_sessions.last_item_seq` is the cached projection; the actor updates
    // it after appending (design §4.4-5), so the row is coherent here too.
    store_rows
        .swarm_update_session(peri_swarm::store::SwarmSessionUpdate {
            logical_id: rows_logical.as_str().to_owned(),
            state: SessionState::Running,
            last_item_seq: rows_last_seq,
            last_activity_at: now(),
        })
        .await
        .expect("project last_item_seq");

    // ── isolation: the state database changed, the session schema did not ────
    let peri_after_swarm = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_after_swarm, peri_before,
        "creating and writing the Swarm state database must leave the session \
         database's catalog and peri_store_meta untouched"
    );
    eprintln!(
        "state store: isolation OK; peri schema_version={} objects={}",
        peri_after_swarm.meta[0].0,
        peri_after_swarm.objects.len()
    );
    assert_eq!(
        assert_swarm_schema(&swarm_probe).await,
        swarm_schema,
        "writing rows does not change the state database's own objects"
    );

    // ── c. close the child and every deployment connection ──────────────────
    let report = child.shutdown().await;
    assert!(report.complete, "host shutdown report: {report:?}");
    drop(reader);
    drop(log);
    close(resources).await;
    state_store
        .close()
        .await
        .expect("close the swarm state store");

    // ── c. reopen both databases with the same DSNs ─────────────────────────
    let reopened = open(&store, &env).await;
    let reopened_state = Arc::new(
        SwarmStateStore::open(&state_store_config(&state_store_dsn))
            .await
            .expect("reopen the independent swarm state store"),
    );
    let reopened_swarm = Arc::clone(&reopened_state);
    let reopened_reader = reopened.session_resources();

    let meta = reopened_reader
        .load_session_meta(&root_session_id)
        .await
        .expect("Peri session metadata after reopen");
    assert_eq!(meta.id, root_session_id);
    assert_eq!(meta.cwd, cwd.to_string_lossy());
    let history = reopened_reader
        .load_session_history(&root_session_id)
        .await
        .expect("Peri history after reopen");
    let texts = history_texts(&history);
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "human" && text.contains("hello postgres state")),
        "user history survives the reopen: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "ai" && text.contains(&format!("{TURN_PREFIX}0"))),
        "assistant history survives the reopen: {texts:?}"
    );

    let mq_rows = reopened_reader
        .mq_query(&root_session_id, MqFilter::default())
        .await
        .expect("MQ rows after reopen");
    assert_eq!(mq_rows.len(), 1, "the durable MQ row survives the reopen");
    assert_eq!(mq_rows[0].id, mq_id);
    assert_eq!(
        mq_rows[0]
            .payload
            .as_message()
            .map(|message| message.content()),
        Some(mq_text.to_owned())
    );

    // Swarm rows: the child session with its real delta/complete item rows, and
    // every row written in `b`.
    let child_session = reopened_swarm
        .swarm_get_session(logical.as_str())
        .await
        .expect("child swarm session after reopen")
        .expect("the child swarm session row survives");
    assert_eq!(child_session.root_session_id, root_session_id);
    assert_eq!(child_session.state, SessionState::Idle);
    assert_eq!(
        child_session.instructions.as_deref(),
        Some("state store run")
    );
    assert_eq!(
        child_session.mcp_tools, declared_mcp,
        "the child session's mcp_json round-trips the canonical declaration"
    );
    let reopened_child_items = reopened_swarm
        .swarm_items_after(logical.as_str(), 0, 100)
        .await
        .expect("child swarm items after reopen");
    assert_eq!(
        reopened_child_items, child_items,
        "the delta rows and the complete row survive the close/reopen unchanged"
    );
    assert_eq!(
        child_session.last_item_seq,
        *seqs.last().expect("at least one row"),
        "the reopened session still projects MAX(seq)"
    );

    let rows_session = reopened_swarm
        .swarm_get_session(rows_logical.as_str())
        .await
        .expect("swarm session after reopen")
        .expect("the swarm session row survives");
    assert_eq!(rows_session.root_session_id, rows_root_session);
    assert_eq!(rows_session.state, SessionState::Running);
    assert_eq!(
        rows_session.instructions.as_deref(),
        Some("state swarm rows")
    );
    assert_eq!(
        rows_session.mcp_tools, declared_mcp,
        "the second session's mcp_json survives the reopen"
    );
    let reopened_mcp: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT mcp_json FROM {}.swarm_sessions WHERE logical_id = $1",
        quote(&schema)
    )))
    .bind(rows_logical.as_str())
    .fetch_all(&swarm_probe)
    .await
    .expect("mcp_json rows");
    assert_eq!(
        reopened_mcp,
        vec![declared_mcp_json.clone()],
        "the persisted mcp_json is still the canonical declaration after reopen"
    );
    assert_eq!(
        reopened_swarm
            .swarm_last_seq(rows_logical.as_str())
            .await
            .expect("last seq after reopen"),
        rows_last_seq
    );
    let items = reopened_swarm
        .swarm_items_after(rows_logical.as_str(), 0, 100)
        .await
        .expect("swarm items after reopen");
    assert_eq!(items.len(), 2);
    assert_eq!(
        items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(items.iter().all(|item| item.payload["state"] == true));
    let newest = reopened_swarm
        .swarm_items_before(rows_logical.as_str(), None, 10)
        .await
        .expect("swarm items descending after reopen");
    assert_eq!(
        newest.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(
        reopened_swarm
            .swarm_get_input(rows_logical.as_str(), &input_row.input_id)
            .await
            .expect("swarm input after reopen"),
        Some(input_row)
    );
    assert_eq!(
        reopened_swarm
            .swarm_get_binding(&binding_env_id)
            .await
            .expect("swarm binding after reopen"),
        Some(binding_row)
    );

    // The reopened state store ran its schema check again; that must not touch
    // the session database either.
    let peri_after_reopen = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_after_reopen, peri_before,
        "reopening both stores leaves the session database's schema untouched"
    );

    // `d`: seq keeps growing after the restart (the storage half of a gateway
    // restart with another input on the same session).
    let next = reopened_swarm
        .swarm_last_seq(rows_logical.as_str())
        .await
        .expect("last seq")
        + 1;
    reopened_swarm
        .swarm_append_item(SwarmItemRow {
            logical_id: rows_logical.as_str().to_owned(),
            seq: next,
            item_id: format!("itim_state_{run}_after_restart"),
            kind: ItemKind::Message,
            source: ItemSource::root(),
            payload: json!({"state": true, "text": "after restart", "complete": true}),
            created_at: now(),
            truncated: false,
        })
        .await
        .expect("append after reopen");
    assert_eq!(
        reopened_swarm
            .swarm_last_seq(rows_logical.as_str())
            .await
            .expect("last seq"),
        next
    );

    close(reopened).await;
    reopened_state
        .close()
        .await
        .expect("close the reopened swarm state store");
    peri_probe.close().await;
    swarm_probe.close().await;
    eprintln!("state store: PostgreSQL run OK");
}

// ── 2. the state locator is mandatory, distinct and actionable ──────────────

/// A missing, shared or unreachable state database must fail closed: the
/// process never substitutes the session database for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires two PostgreSQL databases"]
async fn missing_shared_or_unreachable_state_store_fails_closed() {
    clear_telemetry_env();
    let Some((session_store_dsn, state_store_dsn)) = required_env() else {
        return;
    };
    let (_workspace_temp, root, _cwd) = workspace();

    let mut configuration: BTreeMap<&str, String> = BTreeMap::new();
    configuration.insert(
        API_KEY_ENV,
        "test-deployment-key-not-a-credential".to_owned(),
    );
    configuration.insert(SESSION_STORE_ENV, session_store_dsn.clone());
    configuration.insert(STATE_STORE_ENV, state_store_dsn.clone());
    configuration.insert(MACHINE_ID_ENV, MACHINE_ID.to_owned());
    configuration.insert(WORKSPACE_ROOT_ENV, root.to_string_lossy().into_owned());

    // Missing: a deployment with only the session DSN must not parse.
    let missing = BootstrapConfig::from_env_map(|name| {
        if name == STATE_STORE_ENV {
            None
        } else {
            configuration.get(name).cloned()
        }
    })
    .expect_err("a deployment without a state store must not parse");
    assert!(
        matches!(missing, ConfigError::MissingEnv(STATE_STORE_ENV)),
        "missing state DSN: {missing:?}"
    );
    eprintln!("state store: missing {STATE_STORE_ENV} -> {missing}");

    // Shared: the same database for both would put the Swarm schema back into
    // the session database, so it is rejected as a configuration error.
    let shared = BootstrapConfig::from_env_map(|name| {
        if name == STATE_STORE_ENV {
            Some(session_store_dsn.clone())
        } else {
            configuration.get(name).cloned()
        }
    })
    .expect_err("a shared database must not parse");
    assert!(
        matches!(&shared, ConfigError::Invalid { variable, .. } if *variable == STATE_STORE_ENV),
        "shared DSN: {shared:?}"
    );
    assert!(
        shared.to_string().contains("dedicated PostgreSQL database"),
        "the error must be actionable: {shared}"
    );
    eprintln!("state store: shared DSN -> {shared}");

    // A Loom-era locator is rejected with a migration hint.
    let legacy = BootstrapConfig::from_env_map(|name| {
        if name == STATE_STORE_ENV {
            Some("http://127.0.0.1:9497/db/local_default/swarm/main/".to_owned())
        } else {
            configuration.get(name).cloned()
        }
    })
    .expect_err("a Loom locator must not parse");
    assert!(
        legacy.to_string().contains("no longer supported"),
        "legacy locator: {legacy}"
    );
    eprintln!("state store: legacy locator -> {legacy}");

    // Unreachable: a closed loopback port fails at open with the address in the
    // message and the DSN password nowhere in it.
    let unreachable = SwarmStateStoreConfig {
        locator: PostgresLocator::parse(
            "postgresql://peri:super-secret-password@127.0.0.1:9/swarm_state",
            STATE_LOCATOR_ENV,
        )
        .expect("unreachable state DSN"),
    };
    let error = SwarmStateStore::open(&unreachable)
        .await
        .expect_err("an unreachable state database must fail");
    let message = error.to_string();
    assert!(
        matches!(
            error,
            SwarmStateError::Connect(_) | SwarmStateError::Schema(_)
        ),
        "state store failure class: {error:?}"
    );
    assert!(
        message.contains("127.0.0.1:9/swarm_state"),
        "the failure names the state store target: {message}"
    );
    assert!(
        !message.contains("super-secret-password"),
        "the failure must not echo the DSN password: {message}"
    );
    eprintln!("state store: unreachable state store -> {message}");
}

// ── 3. connection failure is visible and never falls back locally ──────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the remote store integration environment"]
async fn unreachable_session_store_fails_without_creating_a_local_store() {
    clear_telemetry_env();
    let Some((_session_store_dsn, _state_store_dsn)) = required_env() else {
        return;
    };

    let (_workspace_temp, root, cwd) = workspace();
    let machine = machine_id();
    let env = environment(&machine, &root);

    // Closed loopback port: a connection failure, not a locator shape rejection
    // (that path never reaches I/O).
    let unreachable = SessionStoreDeployment::from_locator("postgresql://localhost:9/peri");

    let error = Resources::open_deployment_in_remote_environment(&unreachable, env.clone())
        .await
        .expect_err("an unreachable PostgreSQL server must fail the deployment open");
    let message = format!("{error:#}");
    assert!(
        message.contains("无法打开远程会话存储") || message.contains("remote"),
        "the failure names the remote store: {message}"
    );
    eprintln!("state store failure probe: {message}");

    // The child path must refuse as well: no silent downgrade to SQLite.
    let logical = LogicalId::parse("agent-state-failure").expect("logical id");
    let log = Arc::new(ItemLog::new(
        logical.as_str(),
        Arc::new(peri_swarm::store::MemorySwarmStore::new()),
        broadcast::channel(16).0,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
    ));
    let child_error = AcpChild::spawn(
        ChildConfig {
            cwd: cwd.clone(),
            session_store: unreachable,
            machine_id: Some(machine),
            workspace_root: Some(root.clone()),
            settings_json: Some(settings_json("http://127.0.0.1:9/v1")),
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            workspace: None,
            root_session_id: uuid::Uuid::now_v7().to_string(),
        },
        log,
    )
    .await
    .expect_err("the child must not open a local store when the remote one is unreachable");
    assert!(
        matches!(child_error, ChildError::Local(_)),
        "child failure class: {child_error:?}"
    );
    eprintln!("state store child failure: {child_error}");
}
