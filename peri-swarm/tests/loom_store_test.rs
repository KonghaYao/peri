//! S6b2/S6b3/S6c2: a real Loom deployment with **two** logical databases: the
//! Peri session store (session/history/MQ facts) and the independent Swarm
//! state store (`swarm_meta` + `swarm_*` rows, `design §4.1`, §4.3, §7.2-S6).
//!
//! These tests are ignored by default because they need a running Loom. They are
//! driven purely by environment:
//!
//! - `PERI_SWARM_LOOM_LOCATOR` — the Hrana base URL of the Peri session database
//!   branch, e.g. `http://127.0.0.1:9497/db/local_default/demo/main/`.
//! - `PERI_SWARM_STATE_STORE` — the Hrana base URL of the Swarm state database.
//!   It must be a different logical database (the bootstrap rejects a shared
//!   locator); the test asserts the two locators differ.
//! - `PERI_SWARM_STORE_TOKEN` — the Loom project API key. Only the **name** is
//!   part of the deployment (credential source). The raw schema probes read the
//!   value to authenticate their own connection; it is never printed, asserted,
//!   or written into a failure message.
//! - `HOME` — a scratch directory (set by the harness). The test asserts that no
//!   local Peri session database appears under it.
//!
//! What is real here: Loom, the Peri ACP host, the session/history/MQ writes on
//! the session store, the Swarm rows (including the turn's delta + complete
//! items) on the state store, the closes, and the reopen on new connections.
//! The model endpoint stays a deterministic stub (`design §7.3`).
//!
//! S6c1b rewired this test away from the removed single-database adapter (the
//! S6b path that read `peri-resources`' Swarm port) to [`SwarmStateStore`]
//! opened from `PERI_SWARM_STATE_STORE`. S6c2 is the real dual-database
//! acceptance run: provision the state database with
//! `peri-swarm/scripts/provision_state_db.py`, then run these tests against the
//! two locators it prints.
//!
//! Commands and observed output for the single-database era live in
//! `.peri/plans/S6b2-report.md` and `.peri/plans/S6b3-report.md`; the
//! dual-database run is recorded in `.peri/plans/S6c2-report.md`.

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
use tokio::sync::broadcast;
use url::Url;

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
use peri_swarm::bootstrap::{
    BootstrapConfig, MACHINE_ID_ENV, SESSION_STORE_ENV, STATE_STORE_ENV, STORE_TOKEN_ENV,
};
use peri_swarm::config::{ConfigError, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemSource, LogicalId, McpHeader,
    McpToolSpec, SessionState,
};
use peri_swarm::store::{
    InputPut, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow, SwarmStateError,
    SwarmStateStore, SwarmStateStoreConfig, SwarmStore, SWARM_STATE_SCHEMA_VERSION,
};
use turso_serverless::{Builder, Value as SqlValue};

const LOCATOR_ENV: &str = "PERI_SWARM_LOOM_LOCATOR";
/// Swarm state database locator (`design §4.1`, S6c1a). A separate logical
/// database from `LOCATOR_ENV`; the bootstrap rejects sharing one.
const STATE_LOCATOR_ENV: &str = "PERI_SWARM_STATE_STORE";
/// Credential **source** name. The deployment carries the name; the value stays
/// in the process environment and never reaches an assertion or a log line.
const CREDENTIAL_ENV: &str = "PERI_SWARM_STORE_TOKEN";
/// Optional stable suffix for this run's Swarm identities. Defaults to a fresh
/// UUIDv7 so repeated runs against the same Loom database never collide.
const RUN_ID_ENV: &str = "PERI_SWARM_LOOM_RUN_ID";
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

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} must be set: export the Loom locator/credential before running \
             `cargo test -p peri-swarm --test loom_store_test -- --ignored --nocapture`"
        )
    })
}

fn home_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME must be set to a scratch directory for this test");
    assert!(
        home.is_absolute(),
        "HOME must be absolute: {}",
        home.display()
    );
    home
}

fn local_session_db(home: &Path) -> PathBuf {
    home.join(".peri").join("threads").join("threads.db")
}

fn deployment() -> SessionStoreDeployment {
    SessionStoreDeployment::from_locator(required_env(LOCATOR_ENV))
        .with_engine("turso")
        .with_credential_env(CREDENTIAL_ENV)
}

/// Where the independent Swarm state database is and which variable holds its
/// token. The credential name is shared with the session store, the locator is
/// not.
fn state_store_config() -> SwarmStateStoreConfig {
    SwarmStateStoreConfig {
        locator: Url::parse(&required_env(STATE_LOCATOR_ENV)).expect("state store locator URL"),
        credential_env: CREDENTIAL_ENV.to_owned(),
    }
}

/// The two logical databases must differ (bootstrap rejects a shared locator).
fn assert_distinct_locators() {
    let session = Url::parse(&required_env(LOCATOR_ENV)).expect("session store locator URL");
    let state = Url::parse(&required_env(STATE_LOCATOR_ENV)).expect("state store locator URL");
    assert_ne!(
        session, state,
        "the Swarm state store must be a different logical database from the Peri session store"
    );
}

fn assert_credential_present() {
    assert!(
        std::env::var_os(CREDENTIAL_ENV).is_some(),
        "{CREDENTIAL_ENV} must hold the Loom project API key (the test never reads or prints it)"
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
    let cwd = root.join("agent-s6c2-cwd");
    std::fs::create_dir_all(&cwd).expect("workspace cwd");
    let cwd = std::fs::canonicalize(&cwd).expect("canonical cwd");
    (temp, root, cwd)
}

/// Suffix that keeps this run's Swarm identities distinct from earlier runs on
/// the same Swarm state database.
fn run_id() -> String {
    std::env::var(RUN_ID_ENV).unwrap_or_else(|_| uuid::Uuid::now_v7().to_string())
}

fn environment(machine_id: &uuid::Uuid, root: &Path) -> RemoteWorkspaceEnvironment {
    RemoteWorkspaceEnvironment::virtual_workspace(&machine_id.to_string(), root.to_path_buf())
        .expect("virtual workspace identity")
}

async fn open(store: &SessionStoreDeployment, env: &RemoteWorkspaceEnvironment) -> Resources {
    Resources::open_deployment_in_remote_environment(store, env.clone())
        .await
        .expect("open the Peri session Loom store")
}

/// Consume the deployment shutdown owner: drop the business handle, then close.
async fn close(resources: Resources) {
    let (session, shutdown) = resources.into_parts();
    drop(session);
    shutdown
        .shutdown()
        .await
        .expect("close the remote Loom store");
}

// ── raw schema probes ───────────────────────────────────────────────────────

/// The Peri session schema this build writes and accepts
/// (`peri-resources/src/sessions/canonical.rs`, `CURRENT_SCHEMA_VERSION`, 15
/// since S6c1b). A remote database carries it in `peri_store_meta`.
const PERI_SCHEMA_VERSION: &str = "15";

/// Tables the Swarm state database owns (`peri-swarm/src/store/turso.rs`).
const SWARM_TABLES: [&str; 6] = [
    "swarm_bindings",
    "swarm_inputs",
    "swarm_items",
    "swarm_meta",
    "swarm_sessions",
    "swarm_turns",
];

/// A read-only connection to one logical database. Schema facts
/// (`sqlite_master`, `peri_store_meta`, `swarm_meta`) have no business API, so
/// the test reads them the way an operator would.
///
/// The credential is read from the process environment just to authenticate
/// this connection; it is never printed, compared, or embedded in a panic.
struct SchemaProbe {
    label: &'static str,
    connection: turso_serverless::Connection,
}

impl SchemaProbe {
    async fn open(label: &'static str, locator_env: &str) -> Self {
        let locator = required_env(locator_env);
        let token = std::env::var(CREDENTIAL_ENV).unwrap_or_else(|_| {
            panic!("{label}: {CREDENTIAL_ENV} must hold the Loom project API key")
        });
        let database = Builder::new_remote(&locator)
            .with_auth_token(&token)
            .build()
            .await
            .unwrap_or_else(|error| panic!("{label}: schema probe cannot connect: {error}"));
        let connection = database
            .connect()
            .unwrap_or_else(|error| panic!("{label}: schema probe cannot open: {error}"));
        Self { label, connection }
    }

    async fn rows(&self, sql: &str) -> Vec<Vec<String>> {
        self.rows_with(sql, Vec::new()).await
    }

    async fn rows_with(&self, sql: &str, params: Vec<SqlValue>) -> Vec<Vec<String>> {
        let mut rows = self
            .connection
            .query(sql, params)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{}: schema probe query failed: {error}; sql: {sql}",
                    self.label
                )
            });
        let mut collected = Vec::new();
        loop {
            match rows
                .next()
                .await
                .unwrap_or_else(|error| panic!("{}: schema probe row failed: {error}", self.label))
            {
                Some(row) => collected.push(
                    (0..row.column_count())
                        .map(|index| {
                            let value = row.get_value(index).unwrap_or_else(|error| {
                                panic!("{}: schema probe column failed: {error}", self.label)
                            });
                            render_value(&value)
                        })
                        .collect(),
                ),
                None => return collected,
            }
        }
    }

    async fn close(self) {
        self.connection
            .close()
            .await
            .unwrap_or_else(|error| panic!("{}: schema probe cannot close: {error}", self.label));
    }
}

fn render_value(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "NULL".to_owned(),
        SqlValue::Integer(number) => number.to_string(),
        SqlValue::Real(number) => number.to_string(),
        SqlValue::Text(text) => text.clone(),
        SqlValue::Blob(bytes) => format!("<blob:{}>", bytes.len()),
    }
}

/// Everything the isolation check compares: every schema object (so a stray
/// `swarm_*` table or index shows up) and the single `peri_store_meta` row.
#[derive(Debug, PartialEq)]
struct PeriSchema {
    objects: Vec<Vec<String>>,
    swarm_objects: Vec<Vec<String>>,
    meta: Vec<Vec<String>>,
}

async fn peri_schema(probe: &SchemaProbe) -> PeriSchema {
    PeriSchema {
        objects: probe
            .rows("SELECT type, name, tbl_name FROM sqlite_master ORDER BY type, name")
            .await,
        swarm_objects: probe
            .rows("SELECT type, name FROM sqlite_master WHERE name LIKE 'swarm%' ORDER BY type, name")
            .await,
        meta: probe
            .rows("SELECT singleton, schema_version, store_id, contract FROM peri_store_meta ORDER BY singleton")
            .await,
    }
}

/// The state database owns exactly the Swarm schema, at the version this build
/// writes, and carries no Peri canonical object.
async fn assert_swarm_schema(probe: &SchemaProbe) {
    let versions = probe.rows("SELECT version FROM swarm_meta").await;
    assert_eq!(
        versions,
        vec![vec![SWARM_STATE_SCHEMA_VERSION.to_string()]],
        "swarm_meta holds exactly one version row"
    );

    let tables = probe
        .rows("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'swarm_%' ORDER BY name")
        .await;
    let expected: Vec<Vec<String>> = SWARM_TABLES
        .iter()
        .map(|name| vec![(*name).to_owned()])
        .collect();
    assert_eq!(
        tables, expected,
        "the state database owns exactly the five Swarm tables plus swarm_meta"
    );

    let index = probe
        .rows("SELECT name, tbl_name FROM sqlite_master WHERE type = 'index' AND name = 'swarm_items_by_id'")
        .await;
    assert_eq!(
        index,
        vec![vec![
            "swarm_items_by_id".to_owned(),
            "swarm_items".to_owned()
        ]],
        "the non-unique (logical_id, item_id) index exists on swarm_items"
    );

    let foreign = probe
        .rows("SELECT name FROM sqlite_master WHERE name LIKE 'peri%' OR name IN ('threads', 'messages', 'session_bindings') ORDER BY name")
        .await;
    assert!(
        foreign.is_empty(),
        "the state database must not adopt Peri canonical objects: {foreign:?}"
    );
}

/// Canonical MCP declarations for the round trip: `mcp_json` is the only place
/// the store keeps them, and the value is a test marker, not a credential.
fn declared_mcp_tools() -> Vec<McpToolSpec> {
    let mut tool = McpToolSpec::new("s6c2-state", "https://mcp.example.invalid/state");
    tool.headers.push(McpHeader {
        name: "x-s6c2-run".to_owned(),
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

// ── 1. two logical databases ────────────────────────────────────────────────

/// `a` + `b` + `c` + `e`: the Peri session store carries the session/history/MQ
/// writes of a real turn, the independent Swarm state store carries the
/// session/item/input/binding rows, and both stay readable after the child and
/// every process connection are closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a running Loom; the dual-database acceptance run is S6c2"]
async fn loom_carries_peri_session_history_mq_and_separate_swarm_state() {
    if std::env::var_os("SWARM_TRACE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    clear_telemetry_env();
    assert_credential_present();
    assert_distinct_locators();
    let home = home_dir();
    let local_db = local_session_db(&home);
    assert!(
        !local_db.exists(),
        "precondition: no local session database before the run: {}",
        local_db.display()
    );

    let (base_url, stub) = start_stub().await;
    let (_workspace_temp, root, cwd) = workspace();
    let machine = machine_id();
    let env = environment(&machine, &root);
    let store = deployment();

    // Process-level deployment: Peri reads and MQ writes go to the session
    // database; Swarm rows go to the independent state database with its own
    // `swarm_meta` schema.
    let resources = open(&store, &env).await;

    // ── schema, before the state database exists in this process ────────────
    // The session database carries the Peri schema only, at the version this
    // build writes (15 since S6c1b removed the `swarm_*` tables from it).
    let peri_probe = SchemaProbe::open("peri", LOCATOR_ENV).await;
    let peri_before = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_before.meta.len(),
        1,
        "peri_store_meta holds exactly one row: {:?}",
        peri_before.meta
    );
    assert_eq!(
        peri_before.meta[0][1], PERI_SCHEMA_VERSION,
        "the session database is at the Peri schema version: {:?}",
        peri_before.meta
    );
    assert!(
        peri_before.swarm_objects.is_empty(),
        "the session database must carry no swarm table or index: {:?}",
        peri_before.swarm_objects
    );
    eprintln!(
        "S6c2: peri schema_version={} contract={} schema_objects={} swarm_objects={}",
        peri_before.meta[0][1],
        peri_before.meta[0][3],
        peri_before.objects.len(),
        peri_before.swarm_objects.len()
    );

    // The state store opens a second, independent database. Its first open in a
    // fresh database creates the owned schema and pins `swarm_meta.version`.
    let state_store = Arc::new(
        SwarmStateStore::open(&state_store_config())
            .await
            .expect("open the independent swarm state store"),
    );
    let swarm_probe = SchemaProbe::open("swarm", STATE_LOCATOR_ENV).await;
    assert_swarm_schema(&swarm_probe).await;
    let swarm_schema = swarm_probe
        .rows("SELECT type, name FROM sqlite_master ORDER BY type, name")
        .await;
    eprintln!(
        "S6c2: swarm schema_version={} objects={:?}",
        SWARM_STATE_SCHEMA_VERSION,
        swarm_schema
            .iter()
            .map(|row| format!("{}:{}", row[0], row[1]))
            .collect::<Vec<_>>()
    );

    let store_rows = Arc::clone(&state_store);
    let run = run_id();
    eprintln!("S6c2 run id: {run}");
    let logical = LogicalId::parse(&format!("agent-s6c2-child-{run}")).expect("logical id");
    let root_session_id = uuid::Uuid::now_v7().to_string();
    let created_at = now();
    let declared_mcp = declared_mcp_tools();
    let declared_mcp_json = McpToolSpec::encode_declaration(&declared_mcp)
        .expect("encode the canonical MCP declaration")
        .expect("a non-empty declaration has canonical JSON");

    // The child's item log writes into the **state** database: S4 persists one
    // row per text delta with a stable `itemId`, and the turn terminal adds a
    // complete row with the same id. `swarm_items` allows that repeated id
    // (`item_id` is not unique; S6b3/S6c1a), so this is the real delta + complete
    // projection — not a fabricated row set.
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
            instructions: Some("s6c2 shared loom store".to_owned()),
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
    let request_id = "s6c2-turn-1";
    log.begin_turn(request_id, "turn_s6c2-1")
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
            instructions: Some("s6c2 shared loom store".to_owned()),
            mcp_tools: Vec::new(),
            workspace: None,
            root_session_id: root_session_id.clone(),
        },
        Arc::clone(&log),
    )
    .await
    .expect("spawn the Peri host on the remote Loom store");
    assert_eq!(
        child.session_id(),
        root_session_id,
        "the reserved root session id is the ACP session id"
    );

    let reply = child
        .prompt("hello shared loom", request_id)
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
    // its own strictly increasing seq. With the old `UNIQUE(item_id)` shape the
    // second delta conflicted and the session ended `failed`; both are checked
    // here (state + row shape), not assumed.
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

    // Independent SQL over the state database: the same turn rows exist there,
    // with one repeated item id across several seqs (the deltas and the
    // terminal complete row).
    let item_rows = swarm_probe
        .rows_with(
            "SELECT seq, item_id, payload_json FROM swarm_items WHERE logical_id = ?1 ORDER BY seq",
            vec![SqlValue::Text(logical.as_str().to_owned())],
        )
        .await;
    assert_eq!(
        item_rows.len(),
        child_items.len(),
        "the database holds exactly the rows the store read: {item_rows:?}"
    );
    assert!(
        item_rows.iter().all(|row| row[1] == stream_id),
        "every persisted row reuses item id {stream_id}: {item_rows:?}"
    );
    assert_eq!(
        item_rows
            .iter()
            .filter(|row| row[2].contains("\"complete\":true"))
            .count(),
        1,
        "the complete row is persisted in the state database: {item_rows:?}"
    );
    assert!(
        item_rows
            .iter()
            .filter(|row| row[2].contains("\"delta\":true"))
            .count()
            >= 2,
        "delta rows are persisted in the state database: {item_rows:?}"
    );
    eprintln!(
        "S6c2: swarm item rows={} seqs={:?} item_id={stream_id}",
        item_rows.len(),
        item_rows
            .iter()
            .map(|row| row[0].as_str())
            .collect::<Vec<_>>()
    );

    // `mcp_json` is the canonical declaration text, not a derived shape.
    let mcp_rows = swarm_probe
        .rows_with(
            "SELECT mcp_json FROM swarm_sessions WHERE logical_id = ?1",
            vec![SqlValue::Text(logical.as_str().to_owned())],
        )
        .await;
    assert_eq!(
        mcp_rows,
        vec![vec![declared_mcp_json.clone()]],
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
                    .any(|(role, text)| role == "human" && text.contains("hello shared loom")),
                Err(_) => false,
            }
        }
    })
    .await;

    let meta = reader
        .load_session_meta(&root_session_id)
        .await
        .expect("Peri session metadata from Loom");
    assert_eq!(meta.id, root_session_id);
    assert_eq!(meta.cwd, cwd.to_string_lossy());
    let history = reader
        .load_session_history(&root_session_id)
        .await
        .expect("Peri history from Loom");
    let texts = history_texts(&history);
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "human" && text.contains("hello shared loom")),
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

    // Peri MQ: a durable row through the same remote store. A finished turn
    // hands its input off to history, leaving no MQ row behind, so this row is
    // what makes MQ persistence observable across the reopen.
    let mq_id = MessageId::new();
    let mq_text = "s6c2 durable mq row";
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
    let rows_logical = LogicalId::parse(&format!("agent-s6c2-rows-{run}")).expect("logical id");
    let binding_env_id = format!("logical-env-s6c2-{run}");
    let rows_root_session = uuid::Uuid::now_v7().to_string();
    store_rows
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: rows_logical.as_str().to_owned(),
            root_session_id: rows_root_session.clone(),
            actor_key: format!("agent:{}", rows_logical.as_str()),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Running,
            model: Some("stub-model".to_owned()),
            instructions: Some("s6c2 swarm rows".to_owned()),
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
                item_id: format!("itim_s6c2_{run}_{seq}"),
                kind: ItemKind::Message,
                source: ItemSource::root(),
                payload: json!({
                    "s6c2": true,
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
        input_id: format!("s6c2-input-{run}"),
        fingerprint: "fp-s6c2-1".to_owned(),
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
        sandbox_id: "sandbox-s6c2".to_owned(),
        workspace_path: cwd.to_string_lossy().into_owned(),
        sandbox_root: root.to_string_lossy().into_owned(),
        settings_profile: Some("sonnet".to_owned()),
        token_file: None,
        last_run_id: Some("run-s6c2".to_owned()),
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
         database's sqlite_master and peri_store_meta untouched"
    );
    eprintln!(
        "S6c2: isolation OK; peri schema_version={} objects={} swarm_objects={}",
        peri_after_swarm.meta[0][1],
        peri_after_swarm.objects.len(),
        peri_after_swarm.swarm_objects.len()
    );
    let swarm_schema_after_writes = swarm_probe
        .rows("SELECT type, name FROM sqlite_master ORDER BY type, name")
        .await;
    assert_eq!(
        swarm_schema_after_writes, swarm_schema,
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
    assert!(
        !local_db.exists(),
        "the remote deployment must not create a local session database: {}",
        local_db.display()
    );

    // ── c. reopen both databases with the same locators/credential ──────────
    let reopened = open(&store, &env).await;
    let reopened_state = Arc::new(
        SwarmStateStore::open(&state_store_config())
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
            .any(|(role, text)| role == "human" && text.contains("hello shared loom")),
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
        Some("s6c2 shared loom store")
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
        Some("s6c2 swarm rows")
    );
    assert_eq!(
        rows_session.mcp_tools, declared_mcp,
        "the second session's mcp_json survives the reopen"
    );
    let reopened_mcp = swarm_probe
        .rows_with(
            "SELECT mcp_json FROM swarm_sessions WHERE logical_id = ?1",
            vec![SqlValue::Text(rows_logical.as_str().to_owned())],
        )
        .await;
    assert_eq!(
        reopened_mcp,
        vec![vec![declared_mcp_json.clone()]],
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
    assert!(items.iter().all(|item| item.payload["s6c2"] == true));
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

    close(reopened).await;
    reopened_state
        .close()
        .await
        .expect("close the reopened swarm state store");

    // The reopened state store ran its schema check again; that must not touch
    // the session database either.
    let peri_after_reopen = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_after_reopen, peri_before,
        "reopening both stores leaves the session database's schema untouched"
    );
    peri_probe.close().await;
    swarm_probe.close().await;

    // ── e. no local session database was ever created ───────────────────────
    assert!(
        !local_db.exists(),
        "no local Peri session database under HOME after the whole run: {}",
        local_db.display()
    );
    eprintln!(
        "S6c2: dual-database Loom run OK; checked absent: {}",
        local_db.display()
    );
}

// ── 2. the second locator is mandatory ──────────────────────────────────────

/// A missing or unreachable Swarm state locator must fail closed: the process
/// never substitutes the session database for it, and no local SQLite store is
/// created. `bootstrap::run` maps the open failure to `StateStoreOpen` and
/// closes the session store instead of continuing on one database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running Loom for the session-store half of the probe"]
async fn missing_or_unreachable_state_locator_fails_closed() {
    clear_telemetry_env();
    assert_credential_present();
    let home = home_dir();
    let local_db = local_session_db(&home);
    assert!(
        !local_db.exists(),
        "precondition: no local session database before the failure probe: {}",
        local_db.display()
    );

    let (_workspace_temp, root, _cwd) = workspace();
    let machine = machine_id();
    let env = environment(&machine, &root);
    let session_locator = required_env(LOCATOR_ENV);
    let mut configuration: BTreeMap<&str, String> = BTreeMap::new();
    configuration.insert(
        API_KEY_ENV,
        "test-deployment-key-not-a-credential".to_owned(),
    );
    configuration.insert(SESSION_STORE_ENV, session_locator.clone());
    configuration.insert(STATE_STORE_ENV, required_env(STATE_LOCATOR_ENV));
    configuration.insert(STORE_TOKEN_ENV, CREDENTIAL_ENV.to_owned());
    // The credential value goes into the map (the parser validates that the
    // named variable is non-empty); it is never printed.
    configuration.insert(CREDENTIAL_ENV, required_env(CREDENTIAL_ENV));
    configuration.insert(MACHINE_ID_ENV, MACHINE_ID.to_owned());
    configuration.insert(WORKSPACE_ROOT_ENV, root.to_string_lossy().into_owned());

    // Missing: a deployment with only the session locator must not parse.
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
        "missing state locator: {missing:?}"
    );
    eprintln!("S6c2: missing {STATE_STORE_ENV} -> {missing}");

    // Shared: the same database for both would put the Swarm schema back into
    // the session database, so it is rejected as a configuration error.
    let shared = BootstrapConfig::from_env_map(|name| {
        if name == STATE_STORE_ENV {
            Some(session_locator.clone())
        } else {
            configuration.get(name).cloned()
        }
    })
    .expect_err("a shared locator must not parse");
    assert!(
        matches!(&shared, ConfigError::Invalid { variable, .. } if *variable == STATE_STORE_ENV),
        "shared locator: {shared:?}"
    );
    eprintln!("S6c2: shared locator -> {shared}");

    // Unreachable: the session store is real and opens, then the state store
    // fails on its own connection. Keeping the session database open here shows
    // that the failure cannot degrade into single-database operation.
    let resources = open(&deployment(), &env).await;
    let unreachable = SwarmStateStoreConfig {
        locator: Url::parse("http://127.0.0.1:9/db/local_default/swarm/main/")
            .expect("unreachable state locator"),
        credential_env: CREDENTIAL_ENV.to_owned(),
    };
    let error = SwarmStateStore::open(&unreachable)
        .await
        .expect_err("an unreachable state database must fail");
    let message = error.to_string();
    assert!(
        message.contains("127.0.0.1:9"),
        "the failure names the state store: {message}"
    );
    let credential = required_env(CREDENTIAL_ENV);
    assert!(
        !message.contains(&credential),
        "the failure must not echo the credential"
    );
    assert!(
        matches!(
            error,
            SwarmStateError::Connect(_) | SwarmStateError::Schema(_)
        ),
        "state store failure class: {error:?}"
    );
    close(resources).await;
    assert!(
        !local_db.exists(),
        "a failed state-store open must not create {}",
        local_db.display()
    );
    eprintln!("S6c2: unreachable state store -> {message}");
}

// ── 3. connection failure is visible and never falls back locally ──────────

/// `d`: an unreachable locator fails the deployment **and** the child open, and
/// neither path creates a local SQLite session database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running Loom (see .peri/plans/S6b3-report.md)"]
async fn unreachable_loom_fails_without_creating_a_local_store() {
    clear_telemetry_env();
    assert_credential_present();
    let home = home_dir();
    let local_db = local_session_db(&home);
    assert!(
        !local_db.exists(),
        "precondition: no local session database before the failure probe: {}",
        local_db.display()
    );

    let (_workspace_temp, root, cwd) = workspace();
    let machine = machine_id();
    let env = environment(&machine, &root);

    // Closed loopback port: the failure is a connection failure, not a locator
    // shape rejection (that path never reaches I/O).
    let unreachable =
        SessionStoreDeployment::from_locator("http://127.0.0.1:9/db/local_default/demo/main/")
            .with_engine("turso")
            .with_credential_env(CREDENTIAL_ENV);

    let error = Resources::open_deployment_in_remote_environment(&unreachable, env.clone())
        .await
        .expect_err("an unreachable Loom must fail the deployment open");
    let message = format!("{error:#}");
    assert!(
        message.contains("无法打开远程会话存储") || message.contains("remote"),
        "the failure names the remote store: {message}"
    );
    assert!(
        !local_db.exists(),
        "a failed remote open must not create {}",
        local_db.display()
    );
    eprintln!("S6c2 failure probe: {message}");

    // The child path must refuse as well: no silent downgrade to SQLite.
    let logical = LogicalId::parse("agent-s6c2-failure").expect("logical id");
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
    assert!(
        !local_db.exists(),
        "the child failure must not create {}",
        local_db.display()
    );
    eprintln!("S6c2 child failure: {child_error}");

    assert!(
        !local_db.exists(),
        "no local Peri session database under HOME after the failure paths: {}",
        local_db.display()
    );
    eprintln!(
        "S6c2: failure path OK; checked absent: {}",
        local_db.display()
    );
}
