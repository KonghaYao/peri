//! S8a3a: `environment.type=openai_hosted` binds a Bun workspace.
//!
//! The Bun bridge is a local Axum fixture that implements exactly the contract
//! the actor consumes: lazy reservation (`POST /v1/sandboxes` with
//! `{"lazy":true}`) and the MCP endpoint the injected `workspace` entry points
//! at. The fake never starts SRT; the assertions cover the reserve request, the
//! `swarm_bindings` row, the injected `session/new.mcpServers` entry (observed
//! as the workspace MCP handshake), caller MCP preservation, and the diagnostics
//! for an unbound environment or a missing bridge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

use peri_swarm::actor::service::{AcpSwarmService, ActorServiceConfig};
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::sandbox::{BunSandboxClient, SandboxClientConfig};
use peri_swarm::store::{MemorySwarmStore, SwarmStore};

const TOKEN: &str = "test-deployment-key";
const OUTER_TOKEN: &str = "outer-bun-token";
const UNREACHABLE_MODEL: &str = "http://127.0.0.1:9/v1";

// ── Bun bridge fixture ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ReserveCall {
    body: Value,
    authorization: Option<String>,
}

#[derive(Clone, Debug)]
struct McpCall {
    path: String,
    authorization: Option<String>,
    method: String,
}

#[derive(Default)]
struct BunStub {
    reserves: Mutex<Vec<ReserveCall>>,
    mcp: Mutex<Vec<McpCall>>,
    root: PathBuf,
}

impl BunStub {
    fn sandbox_id_for(logical_env_id: &str) -> String {
        let digest = Sha256::digest(logical_env_id.as_bytes());
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("sbx_{}", &hex[..32])
    }

    fn mcp_methods(&self, path: &str) -> Vec<String> {
        self.mcp
            .lock()
            .iter()
            .filter(|call| call.path == path)
            .map(|call| call.method.clone())
            .collect()
    }

    fn handshake(&self, path: &str) -> Option<McpCall> {
        self.mcp
            .lock()
            .iter()
            .find(|call| {
                call.path == path
                    && (call.method == "server/discover" || call.method == "initialize")
            })
            .cloned()
    }
}

async fn reserve(State(stub): State<Arc<BunStub>>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    stub.reserves.lock().push(ReserveCall {
        body: body.clone(),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    });
    let logical_env_id = body["logicalEnvId"].as_str().unwrap_or_default().to_owned();
    let sandbox_id = BunStub::sandbox_id_for(&logical_env_id);
    let workspace = stub
        .root
        .join(&sandbox_id)
        .join("workspace")
        .to_string_lossy()
        .into_owned();
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1:8081");
    let response = json!({
        "sandbox": {
            "logicalEnvId": logical_env_id,
            "sandboxId": sandbox_id,
            "state": "reserved",
            "runId": null,
            "workspacePath": workspace,
            "mcpUrl": format!("http://{host}/sandboxes/{logical_env_id}/mcp"),
            "lastUsedAt": "2026-11-20T00:00:00Z",
            "lastError": null,
        }
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        response.to_string(),
    )
        .into_response()
}

async fn mcp(
    State(stub): State<Arc<BunStub>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = message["method"].as_str().unwrap_or_default().to_owned();
    stub.mcp.lock().push(McpCall {
        path: uri.path().to_owned(),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        method: method.clone(),
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
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": "bun-workspace-fixture", "version": "1.0.0"}
            }
        }),
        "server/discover" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": "bun-workspace-fixture", "version": "1.0.0"},
                "ttlMs": 0,
                "cacheScope": "private"
            }
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [{
                "name": "lookup",
                "description": "Bun workspace lookup",
                "inputSchema": {"type": "object", "properties": {}}
            }]}
        }),
        "resources/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"resources": []}}),
        "prompts/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"prompts": []}}),
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

async fn start_bun_stub(root: PathBuf) -> (String, Arc<BunStub>) {
    let stub = Arc::new(BunStub {
        root,
        ..BunStub::default()
    });
    let router = Router::new()
        .route("/v1/sandboxes", post(reserve))
        .route("/sandboxes/{logicalEnvId}/mcp", post(mcp))
        .route("/caller/mcp", post(mcp))
        .with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind Bun fixture");
    let addr = listener.local_addr().expect("Bun fixture addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), stub)
}

// ── harness ─────────────────────────────────────────────────────────────────

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
    store: Arc<MemorySwarmStore>,
    workspace: tempfile::TempDir,
    _store_dir: tempfile::TempDir,
}

impl Harness {
    fn new(bun: Option<(&str, Option<&str>)>) -> Self {
        clear_telemetry_env();
        let workspace = tempfile::tempdir().expect("workspace");
        let store_dir = tempfile::tempdir().expect("store dir");
        let env = HashMap::from([
            (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
            (
                WORKSPACE_ROOT_ENV.to_owned(),
                workspace.path().to_string_lossy().into_owned(),
            ),
        ]);
        let config =
            Arc::new(SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config"));
        let store = Arc::new(MemorySwarmStore::new());
        let sandbox = bun.map(|(base_url, token)| {
            BunSandboxClient::new(SandboxClientConfig {
                base_url: Url::parse(base_url).expect("Bun URL"),
                token: token.map(str::to_owned),
            })
        });
        let service = Arc::new(AcpSwarmService::new(
            store.clone(),
            ActorServiceConfig {
                settings_json: Some(settings_json(UNREACHABLE_MODEL)),
                session_store_path: Some(store_dir.path().join("threads.db")),
                session_store: None,
                machine_id: None,
                workspace_root: None,
                model: None,
                channel_capacity: 256,
                sandbox,
            },
        ));
        Self {
            router: build_router(AppState::new(service, config)),
            store,
            workspace,
            _store_dir: store_dir,
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
}

async fn wait_for<F>(stub: &BunStub, within: Duration, predicate: F) -> bool
where
    F: Fn(&BunStub) -> bool,
{
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate(stub) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

// ── tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_anonymous_hosted_session_reserves_a_bun_workspace() {
    let root = tempfile::tempdir().expect("sandbox root");
    let (bun_url, stub) = start_bun_stub(root.path().to_path_buf()).await;
    let harness = Harness::new(Some((&bun_url, Some(OUTER_TOKEN))));

    let (status, value) = harness
        .create_session(
            "agent-hosted",
            json!({
                "environment": { "type": "openai_hosted" },
                "tools": [{
                    "type": "mcp",
                    "server_label": "deepwiki",
                    "server_url": format!("{bun_url}/caller/mcp"),
                }]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let env_id = value["environment"]["id"]
        .as_str()
        .expect("environment id")
        .to_owned();
    assert_eq!(value["environment"]["type"], json!("openai_hosted"));
    let suffix = env_id.strip_prefix("env_").expect("env_ prefix");
    Uuid::parse_str(suffix).expect("anonymous environment id is a UUID");

    let reserves = stub.reserves.lock().clone();
    assert_eq!(reserves.len(), 1, "{reserves:?}");
    assert_eq!(reserves[0].body["logicalEnvId"], json!(env_id));
    assert_eq!(reserves[0].body["lazy"], json!(true));
    assert_eq!(
        reserves[0].authorization,
        Some(format!("Bearer {OUTER_TOKEN}"))
    );

    let binding = harness
        .store
        .swarm_get_binding(&env_id)
        .await
        .expect("get binding")
        .expect("binding row");
    assert_eq!(binding.logical_env_id, env_id);
    assert_eq!(binding.logical_id, "agent-hosted");
    assert_eq!(binding.sandbox_id, BunStub::sandbox_id_for(&env_id));
    assert_eq!(
        binding.workspace_path,
        root.path()
            .join(&binding.sandbox_id)
            .join("workspace")
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(binding.last_run_id, None);

    let workspace_path = format!("/sandboxes/{env_id}/mcp");
    assert!(
        wait_for(&stub, Duration::from_secs(5), |stub| {
            stub.handshake(&workspace_path).is_some() && stub.handshake("/caller/mcp").is_some()
        })
        .await,
        "MCP handshakes missing: {:?}",
        stub.mcp.lock()
    );
    let handshake = stub
        .handshake(&workspace_path)
        .expect("workspace handshake");
    assert_eq!(
        handshake.authorization,
        Some(format!("Bearer {OUTER_TOKEN}"))
    );
    assert!(stub
        .mcp_methods(&workspace_path)
        .iter()
        .any(|method| method == "tools/list"));
    let caller = stub.handshake("/caller/mcp").expect("caller handshake");
    assert_eq!(caller.authorization, None);
    assert!(stub
        .mcp_methods("/caller/mcp")
        .iter()
        .any(|method| method == "tools/list"));
}

#[tokio::test]
async fn an_explicit_environment_id_reuses_the_same_bun_binding() {
    let root = tempfile::tempdir().expect("sandbox root");
    let (bun_url, stub) = start_bun_stub(root.path().to_path_buf()).await;
    let harness = Harness::new(Some((&bun_url, Some(OUTER_TOKEN))));

    let (status, first) = harness
        .create_session(
            "agent-first",
            json!({ "environment": { "type": "openai_hosted" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let env_id = first["environment"]["id"]
        .as_str()
        .expect("environment id")
        .to_owned();
    let first_binding = harness
        .store
        .swarm_get_binding(&env_id)
        .await
        .expect("get binding")
        .expect("binding row");

    let (status, second) = harness
        .create_session(
            "agent-second",
            json!({ "environment": { "type": "openai_hosted", "environment_id": env_id } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    assert_eq!(second["environment"]["id"], json!(env_id));

    let reserves = stub.reserves.lock().clone();
    assert_eq!(reserves.len(), 2, "{reserves:?}");
    assert_eq!(reserves[1].body["logicalEnvId"], json!(env_id));
    assert_eq!(reserves[1].body["lazy"], json!(true));
    let reused = harness
        .store
        .swarm_get_binding(&env_id)
        .await
        .expect("get binding")
        .expect("binding row");
    assert_eq!(reused.sandbox_id, first_binding.sandbox_id);
    assert_eq!(reused.workspace_path, first_binding.workspace_path);
    assert_eq!(reused.logical_id, "agent-second");
}

#[tokio::test]
async fn an_unknown_environment_id_is_diagnosable_and_never_reaches_bun() {
    let root = tempfile::tempdir().expect("sandbox root");
    let (bun_url, stub) = start_bun_stub(root.path().to_path_buf()).await;
    let harness = Harness::new(Some((&bun_url, Some(OUTER_TOKEN))));

    let (status, value) = harness
        .create_session(
            "agent-unknown",
            json!({
                "environment": {
                    "type": "openai_hosted",
                    "environment_id": "env_missing"
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"]["code"], json!("invalid_input"));
    assert_eq!(
        value["error"]["details"]["field"],
        json!("environment.environment_id")
    );
    assert_eq!(value["error"]["details"]["reason"], json!("not_bound"));
    assert_eq!(stub.reserves.lock().len(), 0);
}

#[tokio::test]
async fn a_caller_workspace_label_is_a_real_duplicate_conflict() {
    let root = tempfile::tempdir().expect("sandbox root");
    let (bun_url, stub) = start_bun_stub(root.path().to_path_buf()).await;
    let harness = Harness::new(Some((&bun_url, Some(OUTER_TOKEN))));

    let (status, value) = harness
        .create_session(
            "agent-duplicate",
            json!({
                "environment": { "type": "openai_hosted" },
                "tools": [{
                    "type": "mcp",
                    "server_label": "workspace",
                    "server_url": format!("{bun_url}/caller/mcp"),
                }]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], json!("identity_conflict"));
    assert_eq!(value["error"]["details"]["field"], json!("tools"));
    assert_eq!(value["error"]["details"]["serverLabel"], json!("workspace"));
    assert_eq!(
        value["error"]["details"]["reason"],
        json!("duplicate_server_label")
    );
    assert_eq!(stub.reserves.lock().len(), 0);
}

#[tokio::test]
async fn a_hosted_session_without_a_bun_bridge_fails_diagnosably() {
    let harness = Harness::new(None);
    let (status, value) = harness
        .create_session(
            "agent-no-bridge",
            json!({ "environment": { "type": "openai_hosted" } }),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
    assert_eq!(value["error"]["code"], json!("upstream_unavailable"));
    assert_eq!(
        value["error"]["details"]["operation"],
        json!("sandbox_bridge_unconfigured")
    );
}

#[tokio::test]
async fn a_none_environment_never_calls_the_bun_bridge() {
    let root = tempfile::tempdir().expect("sandbox root");
    let (bun_url, stub) = start_bun_stub(root.path().to_path_buf()).await;
    let harness = Harness::new(Some((&bun_url, Some(OUTER_TOKEN))));

    let (status, value) = harness
        .create_session("agent-none", json!({ "environment": { "type": "none" } }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    assert_eq!(value["environment"]["type"], json!("none"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(stub.reserves.lock().len(), 0);
    assert!(stub.mcp.lock().is_empty(), "{:?}", stub.mcp.lock());
}

#[tokio::test]
async fn a_bridge_rejection_is_mapped_to_upstream_unavailable() {
    let reject = Router::new().route(
        "/v1/sandboxes",
        post(|| async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                json!({"error": {"code": "bindings_invalid", "message": "disk"}}).to_string(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind rejecting bridge");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, reject).await;
    });
    let harness = Harness::new(Some((&format!("http://{addr}"), None)));

    let (status, value) = harness
        .create_session(
            "agent-rejected",
            json!({ "environment": { "type": "openai_hosted" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
    assert_eq!(value["error"]["code"], json!("upstream_unavailable"));
    assert_eq!(
        value["error"]["details"]["operation"],
        json!("sandbox_reserve")
    );
    assert_eq!(
        value["error"]["details"]["upstreamCode"],
        json!("bindings_invalid")
    );
}
