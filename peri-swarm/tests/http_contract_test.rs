use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, SSE_HEARTBEAT_MS_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::{McpHeader, McpToolSpec};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::service::{InMemorySwarmService, ServiceConfig, SwarmService};
use peri_swarm::store::{
    InputPut, MemorySwarmStore, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow,
    SwarmSessionRow, SwarmSessionUpdate, SwarmStore,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-deployment-key";

struct Harness {
    router: Router,
    workspace: TempDir,
}

fn config_for(workspace: &TempDir, heartbeat_ms: u64) -> SwarmConfig {
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
        (SSE_HEARTBEAT_MS_ENV.to_owned(), heartbeat_ms.to_string()),
    ]);
    SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config")
}

fn harness_with_service(service: Arc<dyn SwarmService>, heartbeat_ms: u64) -> (Router, TempDir) {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = Arc::new(config_for(&workspace, heartbeat_ms));
    (build_router(AppState::new(service, config)), workspace)
}

fn harness() -> Harness {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = Arc::new(config_for(&workspace, 15_000));
    let service = Arc::new(InMemorySwarmService::in_memory(ServiceConfig::default()));
    let router = build_router(AppState::new(service, config));
    Harness { router, workspace }
}

fn request(
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
    headers: &[(&str, &str)],
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    };
    builder.body(body).expect("request")
}

async fn send(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Value) {
    let response = router
        .clone()
        .oneshot(request(method, uri, body, token, headers))
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, value)
}

async fn create_session(harness: &Harness, body: Value) -> (StatusCode, Value) {
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(body),
        Some(TOKEN),
        &[("openai-beta", "agents=v1")],
    )
    .await;
    (status, value)
}

#[tokio::test]
async fn healthz_is_unauthenticated_and_session_routes_stay_gated() {
    let harness = harness();

    // The process entry's liveness probe carries no contract header and no
    // bearer: it must not require either, and must not open a session path.
    let (status, _, value) = send(&harness.router, Method::GET, "/healthz", None, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({ "status": "ok" }));

    // Adding the probe must not widen the session surface.
    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01",
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(value["error"]["code"], json!("unauthorized"));
}

#[tokio::test]
async fn missing_or_wrong_bearer_is_401_with_contract_header() {
    let harness = harness();
    let body = json!({ "logicalId": "agent-01" });

    let (status, headers, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(body.clone()),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(value["error"]["code"], json!("unauthorized"));
    assert_eq!(
        headers
            .get("x-peri-swarm-contract")
            .expect("contract")
            .to_str()
            .expect("ascii"),
        "1"
    );
    assert_eq!(
        headers
            .get("www-authenticate")
            .expect("challenge")
            .to_str()
            .expect("ascii"),
        "Bearer"
    );

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(body),
        Some("wrong-key"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(value["error"]["code"], json!("unauthorized"));
}

#[tokio::test]
async fn openai_beta_and_contract_headers_are_validated() {
    let harness = harness();
    let body = json!({ "logicalId": "agent-01" });

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(body.clone()),
        Some(TOKEN),
        &[("openai-beta", "assistants=v2")],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("unsupported_beta"));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(body.clone()),
        Some(TOKEN),
        &[("x-peri-swarm-expect", "2")],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["error"]["code"], json!("unsupported_contract"));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(body),
        Some(TOKEN),
        &[("openai-beta", "agents=v1"), ("x-peri-swarm-expect", "1")],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
}

#[tokio::test]
async fn create_returns_the_documented_shape_and_sdk_aliases() {
    let harness = harness();
    let (status, value) = create_session(
        &harness,
        json!({
            "logicalId": "agent-01",
            "agent": { "model": "model-a", "instructions": "be brief" },
            "input": "hello"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    assert_eq!(value["id"], json!("agent-01"));
    assert_eq!(value["object"], json!("agent.session"));
    assert_eq!(value["logicalId"], json!("agent-01"));
    assert_eq!(value["created"], json!(true));
    assert_eq!(value["state"], json!("idle"));
    assert_eq!(value["lastItemSeq"], json!(1));
    assert_eq!(value["agent"]["model"], json!("model-a"));
    assert_eq!(value["agent"]["instructions"], json!("be brief"));
    assert_eq!(value["environment"]["type"], json!("none"));
    assert_eq!(value["sandbox"], json!(null));
    let root_session_id = value["rootSessionId"].as_str().expect("root id");
    assert!(uuid::Uuid::parse_str(root_session_id).is_ok());
    let cwd = value["cwd"].as_str().expect("cwd");
    assert!(std::path::Path::new(cwd).is_absolute());
}

#[tokio::test]
async fn create_is_idempotent_by_logical_id_and_cwd() {
    let harness = harness();
    let (status, first) = create_session(&harness, json!({ "logicalId": "agent-01" })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, second) = create_session(&harness, json!({ "logicalId": "agent-01" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["created"], json!(false));
    assert_eq!(second["rootSessionId"], first["rootSessionId"]);
}

#[tokio::test]
async fn create_rejects_identity_model_and_validation_errors() {
    let harness = harness();
    let workspace = harness.workspace.path().to_string_lossy().into_owned();
    let (status, first) = create_session(
        &harness,
        json!({ "logicalId": "agent-01", "cwd": workspace }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");

    let other = harness.workspace.path().join("other");
    std::fs::create_dir_all(&other).expect("other dir");
    let (status, value) = create_session(
        &harness,
        json!({ "logicalId": "agent-01", "cwd": other.to_string_lossy() }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["error"]["code"], json!("identity_conflict"));

    let (status, value) = create_session(&harness, json!({ "logicalId": "bad id" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("invalid_logical_id"));

    let (status, value) = create_session(
        &harness,
        json!({ "logicalId": "agent-02", "cwd": "/definitely/missing/peri-swarm" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("cwd_not_found"));

    let (status, value) = create_session(
        &harness,
        json!({ "logicalId": "agent-03", "cwd": "relative/path" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("cwd_not_normalizable"));
}

#[tokio::test]
async fn unknown_fields_and_out_of_scope_capabilities_are_diagnosable() {
    let harness = harness();

    let (status, value) = create_session(
        &harness,
        json!({ "logicalId": "agent-01", "metadata": { "team": "x" } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("unknown_field"));
    assert!(value["error"]["message"]
        .as_str()
        .expect("message")
        .contains("metadata"));

    let (status, value) = create_session(
        &harness,
        json!({ "logicalId": "agent-01", "agent": { "model": "m", "extra": true } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("unknown_field"));

    let (status, value) = create_session(
        &harness,
        json!({
            "logicalId": "agent-01",
            "environment": { "type": "openai_hosted" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("mcp_mode_unsupported"));

    let (status, value) = create_session(
        &harness,
        json!({ "logicalId": "agent-01", "environment": { "type": "container" } }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(value["error"]["code"], json!("unsupported_environment"));

    let (status, value) = create_session(
        &harness,
        json!({
            "logicalId": "agent-01",
            "environment": { "type": "none", "environment_id": "env-1" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(value["error"]["code"], json!("unsupported_environment"));
}

#[tokio::test]
async fn instructions_and_model_limits_are_enforced() {
    let harness = harness();
    let long_instructions = "x".repeat(64 * 1024 + 1);
    let (status, value) = create_session(
        &harness,
        json!({ "logicalId": "agent-01", "instructions": long_instructions }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("invalid_input"));
    assert_eq!(value["error"]["details"]["field"], json!("instructions"));

    let configured = Arc::new(InMemorySwarmService::in_memory(ServiceConfig {
        model: Some("deployment-model".to_owned()),
        ..ServiceConfig::default()
    }));
    let (router, _workspace) = harness_with_service(configured, 15_000);
    let (status, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01", "model": "other-model" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("model_not_supported"));
}

#[tokio::test]
async fn get_session_returns_state_or_404() {
    let harness = harness();
    create_session(&harness, json!({ "logicalId": "agent-01" })).await;

    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01",
        None,
        Some(TOKEN),
        &[("openai-beta", "agents=v1")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["logicalId"], json!("agent-01"));
    assert_eq!(value["state"], json!("idle"));
    assert_eq!(value["turn"]["requestId"], json!(null));
    assert_eq!(value["lastItemSeq"], json!(0));

    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/missing",
        None,
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"]["code"], json!("unknown_agent"));
}

#[tokio::test]
async fn event_submission_is_idempotent_and_conflicts_are_visible() {
    let harness = harness();
    create_session(&harness, json!({ "logicalId": "agent-01" })).await;
    let input_id = uuid::Uuid::now_v7().to_string();
    let body = json!({
        "inputId": input_id,
        "kind": "user",
        "mode": "turn",
        "text": "hello"
    });

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(body.clone()),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(value["accepted"], json!(true));
    assert_eq!(value["disposition"], json!("inserted"));
    assert_eq!(value["lastItemSeq"], json!(1));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(body),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(value["disposition"], json!("existing"));
    assert_eq!(value["lastItemSeq"], json!(1));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "inputId": input_id, "text": "different" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["error"]["code"], json!("input_conflict"));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "text": "missing id" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("invalid_input"));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "inputId": uuid::Uuid::now_v7().to_string(), "text": "   " })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("empty_input"));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({
            "inputId": uuid::Uuid::now_v7().to_string(),
            "attachments": [{ "type": "image", "url": "https://example.invalid/x" }]
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("invalid_input"));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({
            "inputId": uuid::Uuid::now_v7().to_string(),
            "text": "hello",
            "unexpected": true
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("unknown_field"));
}

#[tokio::test]
async fn idempotency_key_header_can_supply_the_input_id() {
    let harness = harness();
    create_session(&harness, json!({ "logicalId": "agent-01" })).await;
    let input_id = uuid::Uuid::now_v7().to_string();

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "text": "via header" })),
        Some(TOKEN),
        &[("idempotency-key", input_id.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    assert_eq!(value["inputId"], json!(input_id));

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "inputId": uuid::Uuid::now_v7().to_string(), "text": "x" })),
        Some(TOKEN),
        &[("idempotency-key", input_id.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("invalid_input"));
}

#[tokio::test]
async fn turn_in_progress_suggests_steer() {
    let manual = Arc::new(InMemorySwarmService::in_memory(ServiceConfig {
        turn_policy: peri_swarm::service::TurnPolicy::Manual,
        ..ServiceConfig::default()
    }));
    let (router, _workspace) = harness_with_service(manual, 15_000);
    let (status, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let first = uuid::Uuid::now_v7().to_string();
    let (status, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "inputId": first, "text": "long" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");

    let (status, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "inputId": uuid::Uuid::now_v7().to_string(), "text": "next" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["error"]["code"], json!("turn_in_progress"));
    assert_eq!(value["error"]["details"]["suggest"], json!("steer"));

    let (status, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({
            "inputId": uuid::Uuid::now_v7().to_string(),
            "mode": "steer",
            "text": "adjust"
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
}

#[tokio::test]
async fn unknown_agent_is_404_on_events_and_items() {
    let harness = harness();
    for uri in [
        "/v1/agents/sessions/missing/items",
        "/v1/agents/sessions/missing/events",
    ] {
        let method = Method::GET;
        let (status, _, value) = send(&harness.router, method, uri, None, Some(TOKEN), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(value["error"]["code"], json!("unknown_agent"), "{uri}");
    }

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/missing/events",
        Some(json!({
            "inputId": uuid::Uuid::now_v7().to_string(),
            "text": "hello"
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"]["code"], json!("unknown_agent"));
}

struct FailingStore {
    inner: MemorySwarmStore,
    fail_put_input: bool,
    fail_append_item: bool,
}

#[async_trait]
impl SwarmStore for FailingStore {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> Result<(), StoreError> {
        self.inner.swarm_upsert_session(row).await
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError> {
        self.inner.swarm_get_session(logical_id).await
    }

    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> Result<(), StoreError> {
        self.inner.swarm_update_session(update).await
    }

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
        if self.fail_append_item {
            return Err(StoreError::Unavailable("append disabled".to_owned()));
        }
        self.inner.swarm_append_item(row).await
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        self.inner.swarm_items_after(logical_id, after, limit).await
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        self.inner
            .swarm_items_before(logical_id, before, limit)
            .await
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError> {
        self.inner.swarm_last_seq(logical_id).await
    }

    async fn swarm_put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError> {
        if self.fail_put_input {
            return Err(StoreError::Unavailable("queue disabled".to_owned()));
        }
        self.inner.swarm_put_input(row).await
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<SwarmInputRow>, StoreError> {
        self.inner.swarm_get_input(logical_id, input_id).await
    }

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> Result<(), StoreError> {
        self.inner.swarm_put_binding(row).await
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<SwarmBindingRow>, StoreError> {
        self.inner.swarm_get_binding(logical_env_id).await
    }
}

async fn failing_router(fail_put_input: bool, fail_append_item: bool) -> (Router, TempDir) {
    let store = Arc::new(FailingStore {
        inner: MemorySwarmStore::new(),
        fail_put_input,
        fail_append_item,
    });
    let service = Arc::new(InMemorySwarmService::new(store, ServiceConfig::default()));
    harness_with_service(service, 15_000)
}

#[tokio::test]
async fn queue_failure_maps_to_503_and_is_not_receipted() {
    let (router, _workspace) = failing_router(true, false).await;
    let (_, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(value["created"], json!(true), "{value}");

    let input_id = uuid::Uuid::now_v7().to_string();
    let (status, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({ "inputId": input_id, "text": "hello" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(value["error"]["code"], json!("queue_unavailable"));
}

#[tokio::test]
async fn append_failure_maps_to_502_upstream_unavailable() {
    let (router, _workspace) = failing_router(false, true).await;
    send(
        &router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
        Some(TOKEN),
        &[],
    )
    .await;

    let (status, _, value) = send(
        &router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({
            "inputId": uuid::Uuid::now_v7().to_string(),
            "text": "hello"
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(value["error"]["code"], json!("upstream_unavailable"));
}

// ── caller-declared MCP tools (S8a1) ────────────────────────────────────────

struct StoreHarness {
    router: Router,
    /// Kept alive so the configured workspace root outlives the router.
    _workspace: TempDir,
    store: Arc<MemorySwarmStore>,
}

fn harness_with_store() -> StoreHarness {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = Arc::new(config_for(&workspace, 15_000));
    let store = Arc::new(MemorySwarmStore::new());
    let service = Arc::new(InMemorySwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ServiceConfig::default(),
    ));
    StoreHarness {
        router: build_router(AppState::new(service, config)),
        _workspace: workspace,
        store,
    }
}

fn deepwiki_tools() -> Value {
    json!([{
        "type": "mcp",
        "server_label": "deepwiki",
        "server_url": "https://mcp.example.test/mcp",
        "headers": { "Authorization": "Bearer caller-token" },
        "require_approval": "never",
        "allowed_tools": ["read_wiki_structure"],
        "defer_loading": true
    }])
}

#[tokio::test]
async fn create_accepts_and_persists_caller_declared_mcp_tools() {
    let harness = harness_with_store();
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01", "agent": { "tools": deepwiki_tools() } })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let row = harness
        .store
        .swarm_get_session("agent-01")
        .await
        .expect("store read")
        .expect("row");
    let expected = McpToolSpec {
        headers: vec![McpHeader {
            name: "Authorization".to_owned(),
            value: "Bearer caller-token".to_owned(),
        }],
        require_approval: Some(json!("never")),
        allowed_tools: Some(json!(["read_wiki_structure"])),
        extra: serde_json::Map::from_iter([("defer_loading".to_owned(), json!(true))]),
        ..McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp")
    };
    assert_eq!(row.mcp_tools, vec![expected], "声明必须逐字段落库");
}

#[tokio::test]
async fn create_without_tools_persists_no_mcp_declaration() {
    let harness = harness_with_store();
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let row = harness
        .store
        .swarm_get_session("agent-01")
        .await
        .expect("store read")
        .expect("row");
    assert!(row.mcp_tools.is_empty());
}

#[tokio::test]
async fn repeated_create_is_idempotent_for_the_same_declaration_and_conflicts_on_another() {
    let harness = harness_with_store();
    let (status, _, first) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01", "tools": deepwiki_tools() })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");

    // 同一声明：复用同一会话。
    let (status, _, second) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01", "tools": deepwiki_tools() })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["created"], json!(false));
    assert_eq!(second["rootSessionId"], first["rootSessionId"]);

    // 数组形式的 headers 与对象形式规范化后相同：仍然幂等。
    let (status, _, third) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({
            "logicalId": "agent-01",
            "tools": [{
                "type": "mcp",
                "server_label": "deepwiki",
                "server_url": "https://mcp.example.test/mcp",
                "headers": [{ "name": "Authorization", "value": "Bearer caller-token" }],
                "require_approval": "never",
                "allowed_tools": ["read_wiki_structure"],
                "defer_loading": true
            }]
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{third}");

    // 声明不同：身份冲突，字段路径指向 tools。
    let (status, _, conflict) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({
            "logicalId": "agent-01",
            "tools": [{
                "type": "mcp",
                "server_label": "other",
                "server_url": "https://mcp.example.test/mcp"
            }]
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], json!("identity_conflict"));
    assert_eq!(conflict["error"]["details"]["field"], json!("tools"));
}

#[tokio::test]
async fn malformed_tools_report_field_paths_and_non_mcp_tools_are_a_stated_gap() {
    let harness = harness_with_store();

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({
            "logicalId": "agent-01",
            "tools": [{ "type": "mcp", "server_label": "x" }]
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"]["code"], json!("invalid_input"));
    assert_eq!(
        value["error"]["details"]["field"],
        json!("tools[0].server_url")
    );

    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({
            "logicalId": "agent-01",
            "tools": [{ "type": "function", "name": "run" }]
        })),
        Some(TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{value}");
    assert_eq!(value["error"]["code"], json!("unsupported_tool"));
    assert_eq!(value["error"]["details"]["field"], json!("tools[0].type"));
    assert_eq!(value["error"]["details"]["supported"], json!(["mcp"]));
}
