//! S5: real Rivet engine, gateway adapter, actor-hosted ACP session.
//!
//! Two phases share one process and one engine:
//!
//! 1. serve the registry with pool `peri-swarm-s5`, create a session through
//!    [`RivetSwarmService`], run turns, read items, and consume the actor event
//!    stream (proving incremental SSE delivery: `fetch` returns headers before
//!    any event exists);
//! 2. stop that registry, serve the same pool again, and assert a rebuilt actor
//!    restores the same `MAX(seq)` from the shared store in `on_start`.
//!
//! The model is a local OpenAI-compatible stub (`design §7.3`): the engine,
//! actor lifecycle, action transport, SSE framing and recovery are real; the
//! model answer is not. Engine build: `Rivet 3.1.1` at `127.0.0.1:6420`
//! (recorded in `.peri/plans/S5-report.md`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use parking_lot::Mutex;
use peri_swarm::actor::deployment::{install_deployment, StaticDeployment};
use peri_swarm::actor::service::ActorServiceConfig;
use peri_swarm::actor::swarm::{registry, Info, InfoOutput, SwarmActor, ACTOR_NAME};
use peri_swarm::domain::{
    ActorKey, ContentBlock, Cursor, Disposition, EnvironmentKind, EnvironmentRef, ErrorCode,
    ErrorKind, InputKind, InputMode, InputSubmission, ItemKind, ItemOrder, ItemsWindow, LogicalId,
    McpToolSpec, SessionState, StreamEvent, TurnEvent,
};
use peri_swarm::gateway::{RivetServiceConfig, RivetSwarmService};
use peri_swarm::service::{CreateSessionCommand, SwarmService};
use peri_swarm::store::{MemorySwarmStore, SwarmSessionRow, SwarmStore};
use reqwest::header::HeaderMap;
use reqwest::Method;
use rivetkit::client::{Client, ClientConfig, GetOrCreateOptions};
use rivetkit::{EngineSpawnMode, ServeConfig, TypedActorHandle};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;

const ENDPOINT: &str = "http://127.0.0.1:6420";
const TOKEN: &str = "default";
const NAMESPACE: &str = "default";
/// Both phases use the same pool: phase 2 must recover the *same* actor
/// identity, not a fresh one in a different pool.
const POOL: &str = "peri-swarm-s5";
/// The M1 replica test keeps its actors out of the S5 pool.
const REPLICA_POOL: &str = "peri-swarm-m1";

const TURN_PREFIX: &str = "stub-";

// ── model stub ──────────────────────────────────────────────────────────────

struct StubModel {
    main_calls: AtomicUsize,
    bodies: Mutex<Vec<Value>>,
}

impl StubModel {
    fn new() -> Self {
        Self {
            main_calls: AtomicUsize::new(0),
            bodies: Mutex::new(Vec::new()),
        }
    }

    fn main_calls(&self) -> usize {
        self.main_calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Value> {
        self.bodies.lock().clone()
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
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if is_background_helper(&request) {
        // Peri's prediction helper runs beside the turn; it must not shift the
        // deterministic turn text.
        return sse_response(&[format!("{TURN_PREFIX}helper")], 0, "helper");
    }
    let index = stub.main_calls.fetch_add(1, Ordering::SeqCst);
    stub.bodies.lock().push(request);
    sse_response(
        &[format!("{TURN_PREFIX}{index}")],
        index,
        &format!("main-{index}"),
    )
}

fn is_background_helper(request: &Value) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message
                    .get("content")
                    .is_some_and(|content| content.to_string().contains("<prediction_directive>"))
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

/// Keep the run hermetic: an inherited Langfuse environment would make the ACP
/// host's `shutdown()` depend on a real endpoint.
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

// ── engine / registry harness ───────────────────────────────────────────────

struct ServedRegistry {
    client: Client,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<rivetkit::prelude::Result<()>>,
}

impl ServedRegistry {
    /// Serve [`registry`] on the shared local engine with a fixed pool.
    async fn start() -> Self {
        Self::start_in(POOL, EngineSpawnMode::Always).await
    }

    /// Serve the same actor registry under an explicit pool and engine-spawn
    /// mode. `Never` is how a replica attaches to an engine another process
    /// owns; nothing in the harness spawns in that case.
    async fn start_in(pool: &str, engine_spawn: EngineSpawnMode) -> Self {
        let config = ServeConfig {
            endpoint: ENDPOINT.to_owned(),
            token: Some(TOKEN.to_owned()),
            namespace: NAMESPACE.to_owned(),
            pool_name: pool.to_owned(),
            engine_spawn,
            ..ServeConfig::default()
        };
        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        let registry = registry();
        let task = tokio::spawn(async move {
            // Envoy readiness is not observable through the public API, so the
            // test polls an action (`wait_ready`) instead.
            registry.serve_with_config(config, serve_shutdown).await
        });
        let client = Client::new(
            ClientConfig::new(ENDPOINT)
                .token(TOKEN)
                .namespace(NAMESPACE)
                .pool_name(pool),
        );
        Self {
            client,
            shutdown,
            task,
        }
    }

    fn handle(&self, logical_id: &str) -> TypedActorHandle<SwarmActor> {
        let key: Vec<String> = ActorKey::new(LogicalId::parse(logical_id).expect("logical id"))
            .segments()
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect();
        TypedActorHandle::new(
            self.client
                .get_or_create(ACTOR_NAME, key, GetOrCreateOptions::default())
                .expect("actor handle"),
        )
    }

    /// Poll `info` until the engine routed to a started actor.
    async fn wait_ready(&self, logical_id: &str) -> InfoOutput {
        let handle = self.handle(logical_id);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match handle.send(Info {}).await {
                Ok(info) => return info,
                Err(error) if Instant::now() < deadline => {
                    tracing::debug!(error = %error, "waiting for the actor to start");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => panic!("actor never became ready: {error}"),
            }
        }
    }

    async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.task.await;
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn user_input(id: &str, text: &str) -> InputSubmission {
    InputSubmission {
        input_id: id.to_owned(),
        kind: InputKind::User,
        mode: InputMode::Turn,
        text: Some(text.to_owned()),
        attachments: Vec::<ContentBlock>::new(),
        plugin_selections: Vec::new(),
        original_draft: None,
    }
}

async fn collect_until_terminal(
    receiver: &mut broadcast::Receiver<StreamEvent>,
    within: Duration,
) -> (Vec<peri_swarm::domain::ItemView>, Option<TurnEvent>) {
    let mut items = Vec::new();
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(received) = timeout(remaining.max(Duration::from_millis(1)), receiver.recv()).await
        else {
            break;
        };
        let Ok(event) = received else { break };
        match event {
            StreamEvent::Item(item) => items.push(item),
            StreamEvent::Turn(turn) => {
                let terminal =
                    matches!(turn, TurnEvent::Completed { .. } | TurnEvent::Failed { .. });
                if terminal {
                    return (items, Some(turn));
                }
            }
            StreamEvent::Gap { .. } => {}
        }
    }
    (items, None)
}

fn is_strictly_increasing(items: &[peri_swarm::domain::ItemView]) -> bool {
    items.windows(2).all(|pair| pair[0].seq < pair[1].seq)
}

// ── test ────────────────────────────────────────────────────────────────────

#[ignore = "requires a local Rivet engine; run with --ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actor_hosts_sessions_streams_events_and_recovers_max_seq() {
    if std::env::var_os("SWARM_TRACE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let session_store: PathBuf = tmp.path().join("threads.db");

    // One deployment for the whole process: the gateway and every actor
    // instance share this store and service configuration.
    let deployment = install_deployment(StaticDeployment::new(
        Arc::new(MemorySwarmStore::new()),
        ActorServiceConfig {
            settings_json: Some(settings_json(&base_url)),
            session_store_path: Some(session_store),
            session_store: None,
            machine_id: None,
            workspace_root: None,
            model: None,
            channel_capacity: 512,
            sandbox: None,
        },
    ));
    let store: Arc<dyn SwarmStore> = deployment.store();

    let logical_a = LogicalId::parse("agent-s5-a").expect("logical id");
    let logical_b = LogicalId::parse("agent-s5-b").expect("logical id");

    // ── phase 1: create, turn, stream, items ────────────────────────────────
    let served = ServedRegistry::start().await;
    let info_a = served.wait_ready(logical_a.as_str()).await;
    assert_eq!(info_a.actor_key, ["agent", "agent-s5-a"]);

    // Buffered fetch path (`on_fetch_response` → `ActorHttpResponse::Buffered`).
    let health = served
        .handle(logical_a.as_str())
        .inner()
        .fetch("/health", Method::GET, HeaderMap::new(), None)
        .await
        .expect("buffered health fetch");
    assert_eq!(health.status().as_u16(), 200);
    let health_body: InfoOutput =
        serde_json::from_slice(&health.bytes().await.expect("health body")).expect("health json");
    assert_eq!(health_body.logical_id, "agent-s5-a");
    assert!(!info_a.live, "no ACP host before the session exists");
    assert_eq!(info_a.recovery, None, "no row to recover yet");

    // A second logicalId routes to a different actor (different key and id).
    let info_b = served.wait_ready(logical_b.as_str()).await;
    assert_eq!(info_b.actor_key, ["agent", "agent-s5-b"]);
    let actor_a = served
        .handle(logical_a.as_str())
        .inner()
        .resolve_handle()
        .await
        .expect("resolve actor a");
    let actor_b = served
        .handle(logical_b.as_str())
        .inner()
        .resolve_handle()
        .await
        .expect("resolve actor b");
    assert_ne!(
        actor_a.actor_id(),
        actor_b.actor_id(),
        "two logicalIds must route to two actors"
    );

    let service = RivetSwarmService::new(
        served.client.clone(),
        Arc::clone(&store),
        RivetServiceConfig::default(),
    );

    // `open_stream` runs before create and must reach the same actor. If the
    // engine buffered the actor's SSE response, this call would never return.
    let pre_create = timeout(Duration::from_secs(20), service.open_stream(&logical_a))
        .await
        .expect("open_stream returns as soon as the actor sends headers");
    let (_, mut first_turn) = pre_create.into_parts();

    // S8a1: the caller-declared MCP declaration crosses the gateway/actor
    // action boundary and is persisted; it is not injected into ACP yet.
    let mut declared_mcp = McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp");
    declared_mcp.require_approval = Some(json!("never"));
    declared_mcp.allowed_tools = Some(json!(["read_wiki_structure"]));

    let created = service
        .create_session(CreateSessionCommand {
            logical_id: logical_a.clone(),
            cwd: cwd.clone(),
            model: None,
            instructions: Some("keep it short".to_owned()),
            mcp_tools: vec![declared_mcp.clone()],
            initial_input: Some("hello".to_owned()),
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
        })
        .await
        .expect("create session through the actor");
    assert!(created.created, "first create must create");
    assert_eq!(created.session.cwd, cwd.to_string_lossy());
    assert_eq!(
        created.session.agent.instructions.as_deref(),
        Some("keep it short")
    );
    let root_session_id = created.session.root_session_id.clone();
    let parsed = uuid::Uuid::parse_str(&root_session_id).expect("uuid root session id");
    assert_eq!(
        parsed.get_version(),
        Some(uuid::Version::SortRand),
        "the gateway reserves a UUIDv7 root session id"
    );
    assert_eq!(stub.main_calls(), 1, "one turn request for the first turn");
    // The actor forwards instructions into the real ACP host.
    let recorded = stub.requests();
    assert!(
        recorded[0]
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| messages
                .iter()
                .any(|message| message["content"].to_string().contains("keep it short"))),
        "inline instructions reach the model request through the actor"
    );

    let (first_items, first_terminal) =
        collect_until_terminal(&mut first_turn, Duration::from_secs(30)).await;
    assert!(
        matches!(&first_terminal, Some(TurnEvent::Completed { stop_reason, .. }) if stop_reason == "end_turn"),
        "first turn terminal: {first_terminal:?}"
    );
    assert!(!first_items.is_empty(), "the create turn produced items");
    assert!(
        is_strictly_increasing(&first_items),
        "items are ordered by seq"
    );
    assert!(
        first_items
            .iter()
            .any(|item| { item.kind == ItemKind::Message && item.payload["complete"] == true }),
        "the completed message item is in the stream"
    );

    // A reserved id minted by the caller is the id the actor's ACP host uses
    // (`session/new` returns it, S1 contract).
    let reserved = "0192f0c5-0000-7000-8000-000000000042".to_owned();
    let created_b = service
        .create_session(CreateSessionCommand {
            logical_id: logical_b.clone(),
            cwd: cwd.clone(),
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            initial_input: None,
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: Some(reserved.clone()),
        })
        .await
        .expect("create with a reserved root session id");
    assert!(created_b.created);
    assert_eq!(created_b.session.root_session_id, reserved);
    assert_eq!(created_b.session.logical_id, logical_b.as_str());

    // The reserved root session id survives the second create: the gateway
    // reads `logicalId → rootSessionId` from the store, and the actor treats
    // the repeat as the same session.
    let repeated = service
        .create_session(CreateSessionCommand {
            logical_id: logical_a.clone(),
            cwd: cwd.clone(),
            model: None,
            instructions: Some("keep it short".to_owned()),
            mcp_tools: vec![declared_mcp.clone()],
            initial_input: None,
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
        })
        .await
        .expect("repeat create");
    assert!(!repeated.created, "repeat create must not create");
    assert_eq!(repeated.session.root_session_id, root_session_id);

    // The declaration was persisted through the real actor + store, and a
    // different declaration for the same logicalId is an identity conflict.
    let stored = store
        .swarm_get_session(logical_a.as_str())
        .await
        .expect("read the session row")
        .expect("row");
    assert_eq!(stored.mcp_tools, vec![declared_mcp.clone()]);
    let mut other_mcp = declared_mcp.clone();
    other_mcp.server_label = "other".to_owned();
    let conflict = service
        .create_session(CreateSessionCommand {
            logical_id: logical_a.clone(),
            cwd: cwd.clone(),
            model: None,
            instructions: Some("keep it short".to_owned()),
            mcp_tools: vec![other_mcp],
            initial_input: None,
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
        })
        .await
        .expect_err("different MCP declaration");
    assert_eq!(conflict.code(), ErrorCode::IdentityConflict);
    assert_eq!(conflict.details()["field"], json!("tools"));

    // Live SSE: open the stream before the turn, then submit.
    let after_first = created.session.last_item_seq;
    let subscription = service
        .subscribe(&logical_a, Cursor::new(after_first))
        .await
        .expect("subscribe through the actor");
    let (_, mut live) = subscription.into_parts();
    let submitted = service
        .submit_input(&logical_a, user_input("input-2", "second"))
        .await
        .expect("submit through the actor");
    assert_eq!(submitted.disposition, Disposition::Inserted);
    assert_eq!(stub.main_calls(), 2, "the second turn reached the model");

    let (live_items, live_terminal) =
        collect_until_terminal(&mut live, Duration::from_secs(30)).await;
    assert!(
        matches!(&live_terminal, Some(TurnEvent::Completed { .. })),
        "live turn terminal: {live_terminal:?}"
    );
    assert!(!live_items.is_empty(), "the live stream delivered items");
    assert!(
        live_items.iter().all(|item| item.seq > after_first),
        "live delivery resumes after the cursor"
    );
    assert!(is_strictly_increasing(&live_items));

    // Persisted items and the session view agree on the same MAX(seq).
    let page = service
        .list_items(
            &logical_a,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("list items through the actor");
    let max_seq = page
        .items
        .iter()
        .map(|item| item.seq)
        .max()
        .expect("items exist");
    assert_eq!(page.last_item_seq, max_seq);
    let completed_texts: Vec<String> = page
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Message && item.payload["complete"] == true)
        .map(|item| item.payload["text"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        completed_texts,
        [format!("{TURN_PREFIX}0"), format!("{TURN_PREFIX}1")],
        "both turns are persisted through the actor"
    );
    let view = service.get_session(&logical_a).await.expect("session view");
    assert_eq!(view.last_item_seq, max_seq);
    assert_eq!(view.state, SessionState::Idle);
    assert_eq!(view.root_session_id, root_session_id);

    // Unknown sessions stay 404 and never reach an actor stream.
    let unknown = service
        .subscribe(
            &LogicalId::parse("agent-s5-missing").expect("logical id"),
            Cursor::START,
        )
        .await
        .expect_err("unknown session");
    assert_eq!(unknown.code(), ErrorCode::UnknownAgent);

    // The actor's incremental read (subscribe action) returns the same page.
    let replay = service
        .read_items(&logical_a, 0, ItemsWindow::MAX_LIMIT)
        .await
        .expect("incremental read through the actor");
    assert_eq!(replay.last_seq, max_seq);
    assert!(is_strictly_increasing(&replay.items));

    // ── phase 2: rebuilt actor restores the read-only view ──────────────────
    served.shutdown().await;
    let served = ServedRegistry::start().await;
    let recovered = served.wait_ready(logical_a.as_str()).await;
    let recovery = recovered
        .recovery
        .clone()
        .expect("on_start restored the session row");
    assert_eq!(recovery.root_session_id, root_session_id);
    assert_eq!(
        recovery.last_item_seq, max_seq,
        "on_start restores MAX(seq) from the store"
    );
    assert!(
        !recovered.live,
        "a rebuilt actor restores only the read-only view (no host before the first turn)"
    );

    let service = RivetSwarmService::new(
        served.client.clone(),
        Arc::clone(&store),
        RivetServiceConfig::default(),
    );
    let after_rebuild = service
        .get_session(&logical_a)
        .await
        .expect("view after rebuild");
    assert_eq!(after_rebuild.last_item_seq, max_seq);
    assert_eq!(after_rebuild.root_session_id, root_session_id);
    let page_after = service
        .list_items(
            &logical_a,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items after rebuild");
    assert_eq!(
        page_after
            .items
            .iter()
            .map(|item| item.seq)
            .collect::<Vec<_>>(),
        page.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        "recovery reads the same persisted items"
    );

    // Reconnect after the rebuild: the actor replays `swarm_items` from the
    // store without a live host.
    let replay_after_rebuild = service
        .subscribe(&logical_a, Cursor::START)
        .await
        .expect("actor SSE replay after rebuild");
    let (_, mut rebuilt_stream) = replay_after_rebuild.into_parts();
    let replayed = timeout(Duration::from_secs(20), async {
        let mut items = Vec::new();
        while let Ok(event) = rebuilt_stream.recv().await {
            match event {
                StreamEvent::Item(item) => {
                    items.push(item);
                    if items.len() == page_after.items.len() {
                        return items;
                    }
                }
                StreamEvent::Turn(_) | StreamEvent::Gap { .. } => {}
            }
        }
        items
    })
    .await
    .expect("backlog replay");
    assert_eq!(replayed.len(), page_after.items.len());
    assert!(is_strictly_increasing(&replayed));

    // S9a4b: the first turn after the rebuild attaches the persisted session
    // (`session/load`), then runs exactly that one new turn. History replay is
    // not projected into `swarm_items`, so the old items stay byte-identical
    // and the new items append after them.
    let before_reattach: Vec<(u64, String)> = page_after
        .items
        .iter()
        .map(|item| (item.seq, item.item_id.clone()))
        .collect();
    let resumed = service
        .submit_input(&logical_a, user_input("input-3", "after rebuild"))
        .await
        .expect("the first turn after the rebuild reattaches the persisted session");
    assert_eq!(resumed.disposition, Disposition::Inserted);
    assert_eq!(
        stub.main_calls(),
        3,
        "the reattached turn reached the model exactly once"
    );
    let live_after_reattach = served
        .handle(logical_a.as_str())
        .send(Info {})
        .await
        .expect("info after reattach");
    assert!(
        live_after_reattach.live,
        "the interaction attached a live host"
    );
    let page_after_reattach = service
        .list_items(
            &logical_a,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items after reattach");
    assert_eq!(
        page_after_reattach
            .items
            .iter()
            .take(before_reattach.len())
            .map(|item| (item.seq, item.item_id.clone()))
            .collect::<Vec<_>>(),
        before_reattach,
        "session/load replay must not duplicate or renumber persisted items"
    );
    assert!(
        page_after_reattach.items.len() > before_reattach.len(),
        "the reattached turn appended new items"
    );
    let last_text = page_after_reattach
        .items
        .iter()
        .rev()
        .find(|item| item.kind == ItemKind::Message && item.payload["complete"] == true)
        .and_then(|item| item.payload["text"].as_str())
        .unwrap_or_default();
    assert_eq!(
        last_text,
        format!("{TURN_PREFIX}2"),
        "the third model call belongs to the reattached turn"
    );

    // Identity check: an unknown logicalId still has no session row.
    let missing = service
        .get_session(&LogicalId::parse("agent-s5-nobody").expect("logical id"))
        .await;
    assert_eq!(
        missing.expect_err("unknown session").code(),
        ErrorCode::UnknownAgent
    );

    served.shutdown().await;
}

// ── M1: replica group with an external engine ───────────────────────────────

/// Two registries on one engine, same namespace and pool:
///
/// - replica A serves with `engine_spawn = Always` and owns the engine process;
/// - replica B serves with `engine_spawn = Never` and only attaches.
///
/// The test then stops A and proves that B alone reconstructs the actor from
/// the shared store (`live = false`, the same `MAX(seq)`), replays items from a
/// cursor, and refuses a row whose turn was still `running` with 502
/// `session_resume_required`.
///
/// Evidence limits:
///
/// - `install_deployment` is **process-level**, so both registries share this
///   process's store and actor configuration. That is equivalent to "both
///   replicas read the same Loom databases"; it is **not** "two hosts", and it
///   says nothing about a multi-node engine or TLS.
/// - because the deployment is process-wide, run this test alone in its own
///   process: `--test rivet_actor_test --ignored --exact <this name>`.
#[ignore = "requires a local Rivet engine; run alone with --ignored --exact"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attaching_registry_recovers_actors_started_by_the_spawning_registry() {
    if std::env::var_os("SWARM_TRACE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    clear_telemetry_env();
    assert!(
        peri_swarm::actor::deployment::current_deployment().is_none(),
        "a swarm deployment is already installed: this process-wide state belongs to another \
         test. Run this test alone: --test rivet_actor_test --ignored --exact \
         an_attaching_registry_recovers_actors_started_by_the_spawning_registry"
    );

    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let session_store: PathBuf = tmp.path().join("threads-m1.db");

    let deployment = install_deployment(StaticDeployment::new(
        Arc::new(MemorySwarmStore::new()),
        ActorServiceConfig {
            settings_json: Some(settings_json(&base_url)),
            session_store_path: Some(session_store),
            session_store: None,
            machine_id: None,
            workspace_root: None,
            model: None,
            channel_capacity: 512,
            sandbox: None,
        },
    ));
    let store: Arc<dyn SwarmStore> = deployment.store();

    let logical = LogicalId::parse("agent-m1-a").expect("logical id");
    let running = LogicalId::parse("agent-m1-running").expect("logical id");

    // ── replica A starts the engine and runs the first turn ─────────────────
    let replica_a = ServedRegistry::start_in(REPLICA_POOL, EngineSpawnMode::Always).await;
    replica_a.wait_ready(logical.as_str()).await;
    let service_a = RivetSwarmService::new(
        replica_a.client.clone(),
        Arc::clone(&store),
        RivetServiceConfig::default(),
    );
    let (_, mut first_turn) = timeout(Duration::from_secs(20), service_a.open_stream(&logical))
        .await
        .expect("open_stream returns as soon as the actor sends headers")
        .into_parts();
    let created = service_a
        .create_session(CreateSessionCommand {
            logical_id: logical.clone(),
            cwd: cwd.clone(),
            model: None,
            instructions: Some("m1 replica".to_owned()),
            mcp_tools: Vec::new(),
            initial_input: Some("hello".to_owned()),
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
        })
        .await
        .expect("create through replica A");
    assert!(created.created);
    let (_, terminal) = collect_until_terminal(&mut first_turn, Duration::from_secs(30)).await;
    assert!(
        matches!(&terminal, Some(TurnEvent::Completed { .. })),
        "first turn terminal: {terminal:?}"
    );
    let before = service_a
        .list_items(
            &logical,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items from replica A");
    assert!(!before.items.is_empty(), "the first turn produced items");
    assert_eq!(stub.main_calls(), 1, "exactly one model turn ran on A");

    // ── replica B attaches: same endpoint, namespace and pool ───────────────
    let replica_b = ServedRegistry::start_in(REPLICA_POOL, EngineSpawnMode::Never).await;
    let attached = replica_b.wait_ready(logical.as_str()).await;
    assert!(
        attached.live,
        "B routes to the actor A already started; the live host lives on A"
    );

    // ── stop A: B alone must reconstruct the actor from the shared store ────
    replica_a.shutdown().await;
    let rebuilt = replica_b.wait_ready(logical.as_str()).await;
    let recovery = rebuilt
        .recovery
        .clone()
        .expect("on_start restored the session row on B");
    assert_eq!(recovery.root_session_id, created.session.root_session_id);
    assert_eq!(
        recovery.last_item_seq, before.last_item_seq,
        "the rebuilt actor restores MAX(seq) from the shared store"
    );
    assert!(
        !rebuilt.live,
        "a rebuilt actor restores only the read-only view (no ACP host)"
    );

    let service_b = RivetSwarmService::new(
        replica_b.client.clone(),
        Arc::clone(&store),
        RivetServiceConfig::default(),
    );
    let after = service_b
        .list_items(
            &logical,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items through replica B");
    assert_eq!(
        after.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        before.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        "B reads the same persisted items as A"
    );

    // Cursor replay on the rebuilt actor (no host): the actor replays from the
    // store, so frames arrive without any model or Bun involvement.
    let (_, mut replay) = service_b
        .subscribe(&logical, Cursor::START)
        .await
        .expect("subscribe through replica B")
        .into_parts();
    let replayed = timeout(Duration::from_secs(20), async {
        let mut items = Vec::new();
        while let Ok(event) = replay.recv().await {
            if let StreamEvent::Item(item) = event {
                items.push(item);
                if items.len() == after.items.len() {
                    break;
                }
            }
        }
        items
    })
    .await
    .expect("cursor replay through replica B");
    assert_eq!(
        replayed.iter().map(|item| item.seq).collect::<Vec<_>>(),
        after.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        "replay follows the cursor order"
    );

    // A row whose turn was still running is the known boundary: the rebuilt
    // actor refuses to attach and the gateway maps it to 502.
    store
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: running.as_str().to_owned(),
            root_session_id: "0192f0c4-0000-7000-8000-0000000000bb".to_owned(),
            actor_key: format!("agent/{}", running.as_str()),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Running,
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            environment: EnvironmentRef {
                kind: EnvironmentKind::None,
                id: None,
            },
            created_at: "2026-11-21T00:00:00.000000Z".to_owned(),
            last_activity_at: "2026-11-21T00:00:00.000000Z".to_owned(),
            last_item_seq: 0,
        })
        .await
        .expect("seed a running row");
    let refused = service_b
        .submit_input(&running, user_input("input-m1-running", "resume"))
        .await
        .expect_err("a running row must be refused");
    assert_eq!(refused.kind(), ErrorKind::BadGateway, "{refused:?}");
    assert_eq!(refused.code(), ErrorCode::UpstreamUnavailable);
    assert_eq!(
        refused.details()["operation"],
        serde_json::json!("session_resume_required")
    );
    let row = store
        .swarm_get_session(running.as_str())
        .await
        .expect("read the running row")
        .expect("row");
    assert_eq!(
        row.state,
        SessionState::Running,
        "the refusal leaves the row untouched"
    );

    replica_b.shutdown().await;
}
