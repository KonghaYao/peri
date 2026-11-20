//! S8a2: caller-declared MCP tools reach the in-process ACP session.
//!
//! `POST /v1/agents/sessions` with `agent.tools` is driven through the real HTTP
//! router, [`AcpSwarmService`], and the in-process ACP host. The MCP servers are
//! local Axum fixtures speaking Streamable HTTP, so the assertions cover the
//! real handshake (`server/discover` for MCP 2026-07-28, `initialize` for a
//! legacy peer), tool discovery, and the declared headers rather than a mock of
//! the injection point (`design §5.1`).
//!
//! The `session/new` wire shape (including the `_meta` carrier keys) is pinned
//! by unit tests in `src/actor/runtime_test.rs`; these tests prove the host
//! actually consumes what the child sends.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt;

use peri_swarm::actor::service::{AcpSwarmService, ActorServiceConfig};
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::store::MemorySwarmStore;

const TOKEN: &str = "test-deployment-key";

// ── MCP HTTP fixture ────────────────────────────────────────────────────────

/// One JSON-RPC request as seen by the fixture.
#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    headers: Vec<(String, String)>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct StubMcp {
    /// `true` makes `server/discover` fail so the pool falls back to the legacy
    /// `initialize` handshake; `false` answers the modern 2026-07-28 probe.
    legacy: bool,
    requests: Mutex<Vec<Recorded>>,
}

impl StubMcp {
    fn methods(&self) -> Vec<String> {
        self.requests
            .lock()
            .iter()
            .map(|request| request.method.clone())
            .collect()
    }

    fn for_method(&self, method: &str) -> Vec<Recorded> {
        self.requests
            .lock()
            .iter()
            .filter(|request| request.method == method)
            .cloned()
            .collect()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().len()
    }
}

async fn stub_mcp(State(stub): State<Arc<StubMcp>>, headers: HeaderMap, body: Bytes) -> Response {
    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = message["method"].as_str().unwrap_or_default().to_owned();
    stub.requests.lock().push(Recorded {
        method: method.clone(),
        headers: headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
    });
    if message.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let id = message["id"].clone();
    let response = match method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": message["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or("2026-07-28"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "swarm-mcp-fixture", "version": "1.0.0"}
            }
        }),
        "server/discover" if !stub.legacy => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "swarm-mcp-fixture", "version": "1.0.0"},
                "ttlMs": 0,
                "cacheScope": "private"
            }
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [{
                "name": "lookup",
                "description": "Fixture lookup",
                "inputSchema": {"type": "object", "properties": {}}
            }]}
        }),
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "Method not found"}
        }),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        response.to_string(),
    )
        .into_response()
}

async fn start_mcp_stub(legacy: bool) -> (String, Arc<StubMcp>) {
    let stub = Arc::new(StubMcp {
        legacy,
        ..StubMcp::default()
    });
    let router = Router::new()
        .route("/mcp", post(stub_mcp))
        .with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind MCP fixture");
    let addr = listener.local_addr().expect("MCP fixture addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}/mcp"), stub)
}

async fn wait_for_method(stub: &StubMcp, method: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if !stub.for_method(method).is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// The pool negotiates MCP `2026-07-28`: a modern server is probed with
/// `server/discover`, and a server that rejects that probe is initialized with
/// the legacy `initialize` frame. Either counts as "a handshake happened".
async fn wait_for_handshake(stub: &StubMcp, within: Duration) -> Option<Recorded> {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if let Some(request) = stub
            .for_method("server/discover")
            .into_iter()
            .next()
            .or_else(|| stub.for_method("initialize").into_iter().next())
        {
            return Some(request);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

// ── harness ─────────────────────────────────────────────────────────────────

/// Model endpoint that is never expected to answer: session creation without
/// `input` must not reach the provider.
const UNREACHABLE_MODEL: &str = "http://127.0.0.1:9/v1";

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

struct Harness {
    router: Router,
    workspace: tempfile::TempDir,
}

impl Harness {
    fn new(store_path: std::path::PathBuf) -> Self {
        Self::with_model(store_path, UNREACHABLE_MODEL)
    }

    fn with_model(store_path: std::path::PathBuf, base_url: &str) -> Self {
        let workspace = tempfile::tempdir().expect("workspace");
        let env = HashMap::from([
            (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
            (
                WORKSPACE_ROOT_ENV.to_owned(),
                workspace.path().to_string_lossy().into_owned(),
            ),
        ]);
        let config =
            Arc::new(SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config"));
        let service = Arc::new(AcpSwarmService::new(
            Arc::new(MemorySwarmStore::new()),
            ActorServiceConfig {
                settings_json: Some(settings_json(base_url)),
                session_store_path: Some(store_path),
                session_store: None,
                machine_id: None,
                workspace_root: None,
                model: None,
                channel_capacity: 256,
            },
        ));
        Self {
            router: build_router(AppState::new(service, config)),
            workspace,
        }
    }

    async fn create_session(&self, logical_id: &str, body: Value) -> (StatusCode, Value) {
        let mut body = body;
        body["logicalId"] = json!(logical_id);
        body["cwd"] = json!(self.workspace.path().to_string_lossy());
        let request = Request::builder()
            .method(Method::POST)
            .uri("/v1/agents/sessions")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .header("openai-beta", "agents=v1")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request");
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("create response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("create body");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn submit_turn(&self, logical_id: &str, text: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/v1/agents/sessions/{logical_id}/events"))
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .header("openai-beta", "agents=v1")
            .body(axum::body::Body::from(
                json!({
                    "inputId": "0192f0c5-0000-7000-8000-000000000001",
                    "text": text,
                })
                .to_string(),
            ))
            .expect("request");
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("submit response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("submit body");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }
}

// ── model fixture ───────────────────────────────────────────────────────────

#[derive(Default)]
struct StubModel {
    requests: Mutex<Vec<Value>>,
}

impl StubModel {
    fn tool_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for body in self.requests.lock().iter() {
            let Some(tools) = body.get("tools").and_then(Value::as_array) else {
                continue;
            };
            for tool in tools {
                if let Some(name) = tool["function"]["name"].as_str() {
                    names.push(name.to_owned());
                }
            }
        }
        names
    }

    /// Tool results already delivered back to the model.
    fn tool_results(&self) -> Vec<String> {
        let mut results = Vec::new();
        for body in self.requests.lock().iter() {
            let Some(messages) = body.get("messages").and_then(Value::as_array) else {
                continue;
            };
            for message in messages {
                if message["role"] == "tool" {
                    results.push(message["content"].as_str().unwrap_or_default().to_owned());
                }
            }
        }
        results
    }
}

fn sse_frames(frames: &[Value]) -> Response {
    let mut sse = String::new();
    for frame in frames {
        sse.push_str("data: ");
        sse.push_str(&frame.to_string());
        sse.push_str("\n\n");
    }
    sse.push_str("data: [DONE]\n\n");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        sse,
    )
        .into_response()
}

async fn stub_chat(State(stub): State<Arc<StubModel>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    stub.requests.lock().push(request.clone());
    let already_searched = request["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"));
    if already_searched {
        return sse_frames(&[
            json!({
                "id": "chatcmpl-mcp-stub",
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "stub" } }],
            }),
            json!({
                "id": "chatcmpl-mcp-stub",
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            }),
        ]);
    }
    // External MCP tools are deferred: the model has to discover them through
    // the ToolSearch meta tool before it can reference them.
    sse_frames(&[
        json!({
            "id": "chatcmpl-mcp-stub",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_tool_search",
                    "type": "function",
                    "function": {
                        "name": "ToolSearch",
                        "arguments": "{\"query\":\"select:mcp__alpha__lookup\"}",
                    },
                }]},
            }],
        }),
        json!({
            "id": "chatcmpl-mcp-stub",
            "object": "chat.completion.chunk",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        }),
    ])
}

async fn start_model_stub() -> (String, Arc<StubModel>) {
    let stub = Arc::new(StubModel::default());
    let router = Router::new()
        .fallback(axum::routing::any(stub_chat))
        .with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind model stub");
    let addr = listener.local_addr().expect("model stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}/v1"), stub)
}

async fn wait_for_tool_result(stub: &StubModel, needle: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if stub
            .tool_results()
            .iter()
            .any(|result| result.contains(needle))
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

fn store_path(workspace: &tempfile::TempDir) -> std::path::PathBuf {
    workspace.path().join("threads.db")
}

fn mcp_tool(label: &str, url: &str, authorization: &str) -> Value {
    json!({
        "type": "mcp",
        "server_label": label,
        "server_url": url,
        "headers": {"Authorization": authorization},
        "require_approval": "never",
        "allowed_tools": ["lookup"],
        "defer_loading": true,
    })
}

// ── tests ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caller_mcp_servers_are_initialized_by_the_session_host() {
    clear_telemetry_env();
    // alpha speaks the modern handshake, beta only the legacy one: the same
    // caller declaration must reach both.
    let (alpha_url, alpha) = start_mcp_stub(false).await;
    let (beta_url, beta) = start_mcp_stub(true).await;
    let workspace = tempfile::tempdir().expect("tempdir");
    let harness = Harness::new(store_path(&workspace));

    let (status, body) = harness
        .create_session(
            "agent-s8a2-two-mcp",
            json!({
                "agent": {
                    "tools": [
                        mcp_tool("alpha", &alpha_url, "Bearer alpha-token"),
                        mcp_tool("beta", &beta_url, "Bearer beta-token"),
                    ]
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
    assert_eq!(body["created"], json!(true), "{body}");
    assert_eq!(body["state"], json!("idle"), "{body}");

    for (name, stub) in [("alpha", &alpha), ("beta", &beta)] {
        assert!(
            wait_for_handshake(stub, Duration::from_secs(30))
                .await
                .is_some(),
            "{name} never received a handshake: {:?}",
            stub.methods()
        );
        assert!(
            wait_for_method(stub, "tools/list", Duration::from_secs(30)).await,
            "{name} never received tools/list: {:?}",
            stub.methods()
        );
    }
    assert!(
        wait_for_method(&alpha, "server/discover", Duration::from_secs(5)).await,
        "a modern server is probed with server/discover: {:?}",
        alpha.methods()
    );
    assert!(
        wait_for_method(&beta, "initialize", Duration::from_secs(5)).await,
        "a server without server/discover is initialized the legacy way: {:?}",
        beta.methods()
    );

    let alpha_handshake = &alpha.for_method("server/discover")[0];
    assert_eq!(
        alpha_handshake.header("authorization"),
        Some("Bearer alpha-token"),
        "caller Authorization must reach the MCP server: {:?}",
        alpha_handshake.headers
    );
    let beta_initialize = &beta.for_method("initialize")[0];
    assert_eq!(
        beta_initialize.header("authorization"),
        Some("Bearer beta-token"),
        "each server keeps its own headers: {:?}",
        beta_initialize.headers
    );
    // Discovery uses the declared header too, not just the handshake.
    for request in alpha.for_method("tools/list") {
        assert_eq!(request.header("authorization"), Some("Bearer alpha-token"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn injected_tools_are_discoverable_by_the_agent() {
    clear_telemetry_env();
    let (mcp_url, mcp) = start_mcp_stub(false).await;
    let (model_url, model) = start_model_stub().await;
    let workspace = tempfile::tempdir().expect("tempdir");
    let harness = Harness::with_model(store_path(&workspace), &model_url);

    let (status, body) = harness
        .create_session(
            "agent-s8a2-tool-surface",
            json!({"agent": {"tools": [mcp_tool("alpha", &mcp_url, "Bearer alpha-token")]}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
    assert!(
        wait_for_method(&mcp, "tools/list", Duration::from_secs(30)).await,
        "the declared server was never discovered: {:?}",
        mcp.methods()
    );

    let (status, body) = harness
        .submit_turn("agent-s8a2-tool-surface", "hello")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "submit failed: {body}");

    // Peri exposes external MCP tools as deferred `mcp__{server}__{tool}`
    // tools: they are not in the direct definition list, so the proof is that
    // ToolSearch finds the injected name in the session catalog.
    assert!(
        wait_for_tool_result(&model, "mcp__alpha__lookup", Duration::from_secs(30)).await,
        "the injected tool was not admitted to the session catalog; model saw {:?}, \
         tool results {:?}",
        model.tool_names(),
        model.tool_results()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_without_tools_starts_no_mcp_traffic() {
    clear_telemetry_env();
    let (url, stub) = start_mcp_stub(false).await;
    // The URL is only meaningful if something injects it; the declaration is
    // intentionally absent, so a request here means a leak.
    let _ = url;
    let workspace = tempfile::tempdir().expect("tempdir");
    let harness = Harness::new(store_path(&workspace));

    let (status, body) = harness
        .create_session("agent-s8a2-no-tools", json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
    assert_eq!(body["created"], json!(true), "{body}");

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        stub.request_count(),
        0,
        "a session without `tools` must not start any MCP client: {:?}",
        stub.methods()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_label_workspace_is_injected_instead_of_rejected() {
    clear_telemetry_env();
    let (url, stub) = start_mcp_stub(false).await;
    let workspace = tempfile::tempdir().expect("tempdir");
    let harness = Harness::new(store_path(&workspace));

    let (status, body) = harness
        .create_session(
            "agent-s8a2-workspace-label",
            json!({"agent": {"tools": [mcp_tool("workspace", &url, "Bearer caller-workspace")]}}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "`workspace` is a caller label, not an API-level error: {body}"
    );
    assert_eq!(body["created"], json!(true), "{body}");
    assert!(
        wait_for_handshake(&stub, Duration::from_secs(30))
            .await
            .is_some(),
        "the declared workspace server was never initialized: {:?}",
        stub.methods()
    );
    assert!(
        wait_for_method(&stub, "tools/list", Duration::from_secs(30)).await,
        "the declared workspace server was never discovered: {:?}",
        stub.methods()
    );
    let handshake = wait_for_handshake(&stub, Duration::from_secs(1))
        .await
        .expect("workspace handshake");
    assert_eq!(
        handshake.header("authorization"),
        Some("Bearer caller-workspace")
    );
}
