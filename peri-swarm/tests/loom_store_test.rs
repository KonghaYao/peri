//! S6b2: one real Loom logical database carries Peri session/history/MQ facts
//! **and** the Swarm rows (`design §4.1`, §4.2, §7.2-S6).
//!
//! Both tests are ignored by default because they need a running Loom. They are
//! driven purely by environment:
//!
//! - `PERI_SWARM_LOOM_LOCATOR` — the Hrana base URL of the database branch, e.g.
//!   `http://127.0.0.1:9497/db/local_default/demo/main/`.
//! - `PERI_SWARM_STORE_TOKEN` — the Loom project API key. Only the **name** is
//!   part of the deployment (credential source); the test never reads, prints,
//!   or embeds the value.
//! - `HOME` — a scratch directory (set by the harness). The test asserts that no
//!   local Peri session database appears under it.
//!
//! What is real here: Loom, the Peri ACP host, the session/history/MQ writes,
//! the `swarm_*` rows, the close, and the reopen on a new connection. The model
//! endpoint stays a deterministic stub (`design §7.3`).
//!
//! Commands and observed output live in `.peri/plans/S6b2-report.md`.

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

use peri_acp_types::messages::{BaseMessage, MessageContent, MessageId};
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageSource, MqEnqueueDisposition, MqFilter, MqInput,
};
use peri_acp_types::session_resources::SessionStoreShutdownPort;
use peri_acp_types::store::PersistedPayload;
use peri_resources::{RemoteWorkspaceEnvironment, Resources};
use peri_swarm::actor::runtime::{AcpChild, ChildConfig, ChildError, SessionStoreDeployment};
use peri_swarm::actor::ItemLog;
use peri_swarm::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemSource, LogicalId, SessionState,
};
use peri_swarm::store::{
    InputPut, RemoteSwarmStore, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow,
    SwarmStore,
};

const LOCATOR_ENV: &str = "PERI_SWARM_LOOM_LOCATOR";
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
    let cwd = root.join("agent-s6b2-cwd");
    std::fs::create_dir_all(&cwd).expect("workspace cwd");
    let cwd = std::fs::canonicalize(&cwd).expect("canonical cwd");
    (temp, root, cwd)
}

/// Suffix that keeps this run's Swarm identities distinct from earlier runs on
/// the same Loom database.
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
        .expect("open the shared Loom store")
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

// ── 1. shared logical database ──────────────────────────────────────────────

/// `a` + `b` + `c` + `e`: one Loom database carries the Peri session/history/MQ
/// writes of a real turn **and** the Swarm session/item/input/binding rows, and
/// both stay readable after the child and every process connection are closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a running Loom (see .peri/plans/S6b2-report.md)"]
async fn loom_carries_peri_session_history_mq_and_swarm_rows() {
    if std::env::var_os("SWARM_TRACE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    clear_telemetry_env();
    assert_credential_present();
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

    // Process-level deployment: Swarm rows, Peri reads and MQ writes share this
    // connection with the same locator/credential.
    let resources = open(&store, &env).await;
    let store_rows = Arc::new(RemoteSwarmStore::new(resources.swarm_store()));
    let run = run_id();
    eprintln!("S6b2 run id: {run}");
    let logical = LogicalId::parse(&format!("agent-s6b2-child-{run}")).expect("logical id");
    let root_session_id = uuid::Uuid::now_v7().to_string();
    let created_at = now();

    // The child's item log stays in memory: S4 persists one row per text delta
    // with a stable `itemId`, while schema 16 declares `swarm_items.item_id`
    // UNIQUE. Wiring the two together fails the second delta with a conflict
    // and marks the session `failed`; that incompatibility is reported as an
    // open S6b2 finding (`.peri/plans/S6b2-report.md` §4) instead of being
    // encoded here. This stage's Swarm rows are written through the real
    // `RemoteSwarmStore` below.
    let item_store: Arc<dyn SwarmStore> = Arc::new(peri_swarm::store::MemorySwarmStore::new());

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
            instructions: Some("s6b2 shared loom store".to_owned()),
            environment: EnvironmentRef::none(&logical),
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
    ));

    // ── a. real Peri host on the remote store, one turn ─────────────────────
    let child = AcpChild::spawn(
        ChildConfig {
            cwd: cwd.clone(),
            session_store: store.clone(),
            machine_id: Some(machine),
            workspace_root: Some(root.clone()),
            settings_json: Some(settings_json(&base_url)),
            model: None,
            instructions: Some("s6b2 shared loom store".to_owned()),
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
        .prompt("hello shared loom", "s6b2-turn-1")
        .await
        .expect("run one turn");
    assert_eq!(reply.stop_reason, "end_turn", "turn reply: {reply:?}");
    assert_eq!(
        stub.main_calls(),
        1,
        "one turn request (total requests incl. helpers: {})",
        stub.calls()
    );

    // Peri session + history, read through the same logical database.
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
    let mq_text = "s6b2 durable mq row";
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

    // ── b. Swarm rows through the process-level store ───────────────────────
    let rows_logical = LogicalId::parse(&format!("agent-s6b2-rows-{run}")).expect("logical id");
    let binding_env_id = format!("logical-env-s6b2-{run}");
    let rows_root_session = uuid::Uuid::now_v7().to_string();
    store_rows
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: rows_logical.as_str().to_owned(),
            root_session_id: rows_root_session.clone(),
            actor_key: format!("agent:{}", rows_logical.as_str()),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Running,
            model: Some("stub-model".to_owned()),
            instructions: Some("s6b2 swarm rows".to_owned()),
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
                item_id: format!("itim_s6b2_{run}_{seq}"),
                kind: ItemKind::Message,
                source: ItemSource::root(),
                payload: json!({
                    "s6b2": true,
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
        input_id: format!("s6b2-input-{run}"),
        fingerprint: "fp-s6b2-1".to_owned(),
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
        sandbox_id: "sandbox-s6b2".to_owned(),
        workspace_path: cwd.to_string_lossy().into_owned(),
        sandbox_root: root.to_string_lossy().into_owned(),
        settings_profile: Some("sonnet".to_owned()),
        token_file: None,
        last_run_id: Some("run-s6b2".to_owned()),
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

    // ── c. close the child and every deployment connection ──────────────────
    let report = child.shutdown().await;
    assert!(report.complete, "host shutdown report: {report:?}");
    drop(reader);
    drop(log);
    close(resources).await;
    assert!(
        !local_db.exists(),
        "the remote deployment must not create a local session database: {}",
        local_db.display()
    );

    // ── c. reopen with the same locator/credential ──────────────────────────
    let reopened = open(&store, &env).await;
    let reopened_swarm = Arc::new(RemoteSwarmStore::new(reopened.swarm_store()));
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

    // Swarm rows: the seeded child session, the turn's items, and every row
    // written in `b`.
    // Swarm rows: the session tied to the Peri root id, the items, the input
    // receipt and the binding written in `b`.
    let child_session = reopened_swarm
        .swarm_get_session(logical.as_str())
        .await
        .expect("child swarm session after reopen")
        .expect("the child swarm session row survives");
    assert_eq!(child_session.root_session_id, root_session_id);
    assert_eq!(child_session.state, SessionState::Idle);
    assert_eq!(
        child_session.instructions.as_deref(),
        Some("s6b2 shared loom store")
    );
    assert_eq!(
        reopened_swarm
            .swarm_items_after(logical.as_str(), 0, 100)
            .await
            .expect("child swarm items after reopen"),
        Vec::new(),
        "the in-memory item log wrote no fabricated Loom rows"
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
        Some("s6b2 swarm rows")
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
    assert!(items.iter().all(|item| item.payload["s6b2"] == true));
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

    // ── e. no local session database was ever created ───────────────────────
    assert!(
        !local_db.exists(),
        "no local Peri session database under HOME after the whole run: {}",
        local_db.display()
    );
    eprintln!(
        "S6b2: shared Loom run OK; checked absent: {}",
        local_db.display()
    );
}

// ── 2. connection failure is visible and never falls back locally ──────────

/// `d`: an unreachable locator fails the deployment **and** the child open, and
/// neither path creates a local SQLite session database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the S6b2 environment (see .peri/plans/S6b2-report.md)"]
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
    eprintln!("S6b2 failure probe: {message}");

    // The child path must refuse as well: no silent downgrade to SQLite.
    let logical = LogicalId::parse("agent-s6b2-failure").expect("logical id");
    let log = Arc::new(ItemLog::new(
        logical.as_str(),
        Arc::new(peri_swarm::store::MemorySwarmStore::new()),
        broadcast::channel(16).0,
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
    eprintln!("S6b2 child failure: {child_error}");

    assert!(
        !local_db.exists(),
        "no local Peri session database under HOME after the failure paths: {}",
        local_db.display()
    );
    eprintln!(
        "S6b2: failure path OK; checked absent: {}",
        local_db.display()
    );
}
