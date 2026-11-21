//! Official `sessions` resource surface beyond the original five routes:
//! `sessions.list`/`update`/`delete`, `turns.list`/`retrieve`, and the explicit
//! `404` for official resources the gateway cannot map.
//!
//! The tests drive the real axum router over the in-memory service/store; the
//! actor path is covered by `actor_runtime_test.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::{ItemOrder, SessionsWindow, TurnStatus, TurnsWindow};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::service::{InMemorySwarmService, ServiceConfig, TurnPolicy};
use peri_swarm::store::{
    InputPut, MemorySwarmStore, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemPage,
    SwarmItemRow, SwarmSessionPatch, SwarmSessionRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-deployment-key";

struct Harness {
    router: Router,
    _workspace: TempDir,
    store: Arc<MemorySwarmStore>,
}

fn harness_with(policy: TurnPolicy) -> Harness {
    let workspace = tempfile::tempdir().expect("workspace");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
    ]);
    let config = SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config");
    let store = Arc::new(MemorySwarmStore::new());
    let service = Arc::new(InMemorySwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ServiceConfig {
            turn_policy: policy,
            ..ServiceConfig::default()
        },
    ));
    Harness {
        router: build_router(AppState::new(service, Arc::new(config))),
        _workspace: workspace,
        store,
    }
}

fn harness() -> Harness {
    harness_with(TurnPolicy::Immediate)
}

fn request(method: Method, uri: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
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
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request(method, uri, body, token))
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            panic!(
                "non-JSON response ({status}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, value)
}

/// `POST` with an explicit content-type (or none) and a raw body string, so
/// the "no body at all" shape the official SDK sends stays expressible.
async fn send_raw(
    harness: &Harness,
    uri: &str,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"));
    if let Some(content_type) = content_type {
        builder = builder.header("content-type", content_type);
    }
    let response = harness
        .router
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json")
    };
    (status, value)
}

async fn stored_session(harness: &Harness, logical_id: &str) -> SwarmSessionRow {
    harness
        .store
        .swarm_get_session(logical_id)
        .await
        .expect("row")
        .expect("session")
}

async fn create(harness: &Harness, body: Value) -> (StatusCode, Value) {
    send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(body),
        Some(TOKEN),
    )
    .await
}

fn input_id(index: u64) -> String {
    format!("0192f0c5-0000-7000-8000-{:012}", index)
}

async fn submit(
    harness: &Harness,
    logical_id: &str,
    index: u64,
    text: &str,
) -> (StatusCode, Value) {
    send(
        &harness.router,
        Method::POST,
        &format!("/v1/agents/sessions/{logical_id}/events"),
        Some(json!({ "inputId": input_id(index), "text": text })),
        Some(TOKEN),
    )
    .await
}

fn turn_id(index: u64) -> String {
    format!("turn_input_{}", input_id(index))
}

// ── create / get / list / update round trips ───────────────────────────────

#[tokio::test]
async fn create_get_and_list_round_trip_metadata_and_prompt_cache() {
    let harness = harness();
    let (status, created) = create(
        &harness,
        json!({
            "logicalId": "agent-a",
            "metadata": { "team": "alpha", "env": "dev" },
            "prompt_cache_options": { "ttl": "12h" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["metadata"]["team"], json!("alpha"));
    assert_eq!(created["metadata"]["env"], json!("dev"));
    assert_eq!(created["prompt_cache_options"]["ttl"], json!("12h"));
    assert!(
        created.get("promptCacheOptions").is_none(),
        "the official SDK reads only `prompt_cache_options`: {created}"
    );

    let (status, read) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-a",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["metadata"], created["metadata"]);
    assert_eq!(
        read["prompt_cache_options"],
        created["prompt_cache_options"]
    );
    assert!(read.get("promptCacheOptions").is_none());

    // A second session without metadata proves filters and default order.
    let (status, second) = create(&harness, json!({ "logicalId": "agent-b" })).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    assert_eq!(second["metadata"], json!({}));
    assert_eq!(second["prompt_cache_options"], json!(null));

    let (status, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["object"], json!("list"));
    assert_eq!(page["has_more"], json!(false));
    assert_eq!(page["hasMore"], json!(false));
    assert_eq!(page["nextCursor"], json!(null));
    let ids: Vec<&str> = page["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|session| session["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, vec!["agent-b", "agent-a"], "default order is desc");
    for session in page["data"].as_array().expect("data") {
        assert!(
            session.get("promptCacheOptions").is_none(),
            "list items must use the official spelling: {session}"
        );
    }

    let (status, filtered) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions?metadata%5Bteam%5D=alpha",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{filtered}");
    let data = filtered["data"].as_array().expect("data");
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["id"], json!("agent-a"));

    let (status, by_root) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions?agent_id=root",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_root["data"].as_array().expect("data").len(), 2);

    let (status, other_agent) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions?agent_id=someone-else",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(other_agent["data"].as_array().expect("data").len(), 0);
    assert_eq!(other_agent["has_more"], json!(false));

    // Keyset pagination: `after` is the last id of the previous page.
    let (status, first_page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions?limit=1",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_page["data"].as_array().expect("data").len(), 1);
    assert_eq!(first_page["data"][0]["id"], json!("agent-b"));
    assert_eq!(first_page["has_more"], json!(true));
    assert_eq!(first_page["hasMore"], json!(true));
    assert_eq!(first_page["nextCursor"], json!("agent-b"));

    let (status, second_page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions?limit=1&after=agent-b",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_page["data"][0]["id"], json!("agent-a"));
    assert_eq!(second_page["has_more"], json!(false));
    assert_eq!(second_page["nextCursor"], json!(null));

    let (status, asc) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions?order=asc&limit=1",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(asc["data"][0]["id"], json!("agent-a"));
    assert_eq!(asc["nextCursor"], json!("agent-a"));
}

#[tokio::test]
async fn create_and_retrieve_expose_empty_official_default_fields() {
    let harness = harness();
    let (status, created) = create(&harness, json!({ "logicalId": "agent-defaults" })).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, read) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-defaults",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");

    for (label, value) in [("create", &created), ("retrieve", &read)] {
        assert_eq!(value["required_actions"], json!([]), "{label}: {value}");
        assert_eq!(value["vault_ids"], json!([]), "{label}: {value}");
        assert_eq!(value["usage"], json!(null), "{label}: {value}");
        assert_eq!(value["error"], json!(null), "{label}: {value}");
        assert_eq!(
            value["environment"],
            json!({ "type": "none" }),
            "official EnvironmentResourceNone has only `type`: {label}: {value}"
        );
    }
}

#[tokio::test]
async fn prompt_cache_options_empty_object_uses_the_official_default_ttl() {
    let harness = harness();
    let (status, created) = create(
        &harness,
        json!({
            "logicalId": "agent-default-ttl",
            "prompt_cache_options": {}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["prompt_cache_options"],
        json!({ "ttl": "30m" }),
        "official PromptCacheOptions.ttl defaults to 30m: {created}"
    );

    // Omission still keeps the stored value on attach.
    let (status, attached) = create(&harness, json!({ "logicalId": "agent-default-ttl" })).await;
    assert_eq!(status, StatusCode::OK, "{attached}");
    assert_eq!(attached["prompt_cache_options"], json!({ "ttl": "30m" }));

    // `{}` is an explicit 30m value, so it matches the stored default...
    let (status, same) = create(
        &harness,
        json!({
            "logicalId": "agent-default-ttl",
            "prompt_cache_options": {}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{same}");

    // ...and `null` still means "unset", which conflicts with 30m.
    let (status, error) = create(
        &harness,
        json!({ "logicalId": "agent-default-ttl", "prompt_cache_options": null }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], json!("identity_conflict"));

    // A session with no configured value can adopt the default explicitly;
    // `null` still clears it, and omission keeps whatever is stored.
    let (status, _) = create(&harness, json!({ "logicalId": "agent-unset-ttl" })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, set) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-unset-ttl",
        Some(json!({ "prompt_cache_options": {} })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(set["prompt_cache_options"], json!({ "ttl": "30m" }));

    let (status, kept) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-unset-ttl",
        Some(json!({})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["prompt_cache_options"], json!({ "ttl": "30m" }));

    let (status, cleared) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-unset-ttl",
        Some(json!({ "prompt_cache_options": null })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["prompt_cache_options"], json!(null));
}

#[tokio::test]
async fn session_list_query_and_create_validation_are_explicit() {
    let harness = harness();
    let long_key = "k".repeat(65);
    let long_value = "v".repeat(513);
    let mut too_many = serde_json::Map::new();
    for index in 0..17 {
        too_many.insert(format!("k{index}"), json!("v"));
    }

    for body in [
        json!({ "logicalId": "agent-1", "prompt_cache_options": { "ttl": "5m" } }),
        json!({ "logicalId": "agent-1", "prompt_cache_options": { "ttl": 30 } }),
        json!({ "logicalId": "agent-1", "prompt_cache_options": { "other": true } }),
        json!({ "logicalId": "agent-1", "metadata": { "k": 1 } }),
        json!({ "logicalId": "agent-1", "metadata": { long_key.clone(): "v" } }),
        json!({ "logicalId": "agent-1", "metadata": { "k": long_value.clone() } }),
        json!({ "logicalId": "agent-1", "metadata": Value::Object(too_many) }),
    ] {
        let (status, error) = create(&harness, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert_eq!(error["error"]["code"], json!("invalid_input"), "{error}");
        assert!(
            error["error"]["details"]["field"].as_str().is_some(),
            "validation must name the field: {error}"
        );
    }

    let queries = vec![
        "cursor=whatever".to_owned(),
        "limit=0".to_owned(),
        "limit=501".to_owned(),
        "limit=abc".to_owned(),
        "order=sideways".to_owned(),
        "metadata%5B%5D=x".to_owned(),
        format!("metadata%5Bk%5D={}", "v".repeat(513)),
    ];
    for query in queries {
        let (status, error) = send(
            &harness.router,
            Method::GET,
            &format!("/v1/agents/sessions?{query}"),
            None,
            Some(TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {error}");
        assert_eq!(error["error"]["code"], json!("invalid_query"), "{query}");
    }
}

#[tokio::test]
async fn update_replaces_and_clears_official_fields() {
    let harness = harness();
    create(
        &harness,
        json!({
            "logicalId": "agent-u",
            "model": "model-1",
            "metadata": { "keep": "yes", "drop": "no" },
            "prompt_cache_options": { "ttl": "30m" }
        }),
    )
    .await;

    // The official `sessions.update` verb is POST; PATCH was never part of the
    // official surface and must not stay reachable as a second spelling.
    let (status, _) = send(
        &harness.router,
        Method::PATCH,
        "/v1/agents/sessions/agent-u",
        Some(json!({ "metadata": {} })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    let (status, updated) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-u",
        Some(json!({
            "metadata": { "only": "one" },
            "prompt_cache_options": { "ttl": "12h" }
        })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["metadata"], json!({ "only": "one" }));
    assert_eq!(updated["prompt_cache_options"]["ttl"], json!("12h"));
    assert_eq!(updated["agent"]["model"], json!("model-1"));

    let (status, cleared) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-u",
        Some(json!({ "metadata": null, "prompt_cache_options": null })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["metadata"], json!({}));
    assert_eq!(cleared["prompt_cache_options"], json!(null));

    // Omitted fields keep their value.
    let (status, unchanged) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-u",
        Some(json!({})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    assert_eq!(unchanged["agent"]["model"], json!("model-1"));

    let (status, model) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-u",
        Some(json!({ "agent": { "model": "model-2" } })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{model}");
    assert_eq!(model["agent"]["model"], json!("model-2"));

    // `agent.tools` uses the same normalizer as create; an empty array clears.
    let with_tools = json!([{
        "type": "mcp",
        "server_label": "deepwiki",
        "server_url": "https://mcp.example.test/mcp"
    }]);
    create(
        &harness,
        json!({ "logicalId": "agent-tools", "agent": { "tools": with_tools } }),
    )
    .await;
    assert_eq!(
        harness
            .store
            .swarm_get_session("agent-tools")
            .await
            .expect("row")
            .expect("session")
            .mcp_tools
            .len(),
        1
    );
    let (status, _) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-tools",
        Some(json!({ "agent": { "tools": [] } })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(harness
        .store
        .swarm_get_session("agent-tools")
        .await
        .expect("row")
        .expect("session")
        .mcp_tools
        .is_empty());

    for body in [
        json!({ "review": "auto_review" }),
        json!({ "agent": { "reasoning": { "effort": "high" } } }),
        json!({ "agent": { "service_tier": "flex" } }),
    ] {
        let (status, error) = send(
            &harness.router,
            Method::POST,
            "/v1/agents/sessions/agent-u",
            Some(body),
            Some(TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert_eq!(error["error"]["code"], json!("invalid_request"), "{error}");
        assert_eq!(error["error"]["details"]["supported"], json!(false));
    }

    let (status, error) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-u",
        Some(json!({ "unknown": true })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("unknown_field"));

    let (status, missing) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-missing",
        Some(json!({ "metadata": {} })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["error"]["code"], json!("unknown_agent"));
}

/// Official `sessions.update` allows `update(id)` / `update(id, null)`.
/// Omitted body is sent as `{}` with JSON content-type; an explicit `null`
/// sends no body and no content-type. Both are empty updates, not a `415`,
/// and neither may write any row.
#[tokio::test]
async fn update_accepts_an_absent_or_null_body_as_a_no_op() {
    let harness = harness();
    let (status, created) = create(
        &harness,
        json!({
            "logicalId": "agent-noop",
            "model": "model-1",
            "metadata": { "keep": "yes" },
            "prompt_cache_options": { "ttl": "30m" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let before = stored_session(&harness, "agent-noop").await;

    for (label, content_type, body) in [
        ("no body, no content-type", None, ""),
        ("empty body, text/plain", Some("text/plain"), ""),
        ("JSON null", Some("application/json"), "null"),
    ] {
        let (status, session) = send_raw(
            &harness,
            "/v1/agents/sessions/agent-noop",
            content_type,
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{label}: {session}");
        assert_eq!(session["agent"]["model"], json!("model-1"), "{label}");
        assert_eq!(session["metadata"], json!({ "keep": "yes" }), "{label}");
        assert_eq!(
            session["prompt_cache_options"]["ttl"],
            json!("30m"),
            "{label}"
        );
        // The response alone cannot prove "no write": `update_session` would
        // also return the same values for a keep-only patch. The stored row
        // (including `last_activity_at`) is the evidence.
        let after = stored_session(&harness, "agent-noop").await;
        assert_eq!(after.model, before.model, "{label}");
        assert_eq!(after.mcp_tools, before.mcp_tools, "{label}");
        assert_eq!(after.metadata, before.metadata, "{label}");
        assert_eq!(
            after.prompt_cache_options, before.prompt_cache_options,
            "{label}"
        );
        assert_eq!(after.last_activity_at, before.last_activity_at, "{label}");
    }

    let (status, error) = send_raw(
        &harness,
        "/v1/agents/sessions/agent-noop",
        Some("text/plain"),
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{error}");
    assert_eq!(error["error"]["code"], json!("invalid_content_type"));

    // Malformed JSON is a request error, never a silent no-op.
    let (status, error) = send_raw(
        &harness,
        "/v1/agents/sessions/agent-noop",
        Some("application/json"),
        "{",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("invalid_request"));

    let (status, updated) = send_raw(
        &harness,
        "/v1/agents/sessions/agent-noop",
        Some("application/json"),
        r#"{"metadata":{"only":"one"},"agent":{"model":"model-2"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["metadata"], json!({ "only": "one" }));
    assert_eq!(updated["agent"]["model"], json!("model-2"));
    assert_eq!(updated["prompt_cache_options"]["ttl"], json!("30m"));
}

#[tokio::test]
async fn a_null_agent_model_is_rejected_but_null_tools_clear() {
    let harness = harness();

    for (method, path, body) in [
        (
            Method::POST,
            "/v1/agents/sessions",
            json!({ "logicalId": "agent-null-model", "agent": { "model": null } }),
        ),
        (
            Method::POST,
            "/v1/agents/sessions/agent-u",
            json!({ "agent": { "model": null } }),
        ),
    ] {
        let (status, error) = send(&harness.router, method, path, Some(body), Some(TOKEN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {error}");
        assert_eq!(error["error"]["code"], json!("invalid_request"), "{path}");
        assert_eq!(
            error["error"]["details"]["field"],
            json!("agent.model"),
            "{path}"
        );
    }

    // The session used by the update half must exist first.
    let (status, _) = create(
        &harness,
        json!({
            "logicalId": "agent-u",
            "model": "model-keep",
            "agent": { "tools": [{
                "type": "mcp",
                "server_label": "deepwiki",
                "server_url": "https://mcp.example.test/mcp"
            }] }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // Without this precondition the final `mcp_tools.is_empty()` would pass
    // vacuously if create had dropped the declaration instead of persisting it.
    let before = stored_session(&harness, "agent-u").await;
    assert!(!before.mcp_tools.is_empty(), "{:?}", before.mcp_tools);

    // `agent.tools:null` is the official "clear the declaration" spelling and
    // stays valid.
    let (status, cleared) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-u",
        Some(json!({ "agent": { "tools": null } })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");

    // The 200 alone would also be returned by a keep-only patch; the stored row
    // proves the tools were cleared while the model was left alone.
    let row = stored_session(&harness, "agent-u").await;
    assert!(row.mcp_tools.is_empty(), "{:?}", row.mcp_tools);
    assert_eq!(row.model.as_deref(), Some("model-keep"));
}

#[tokio::test]
async fn create_attach_distinguishes_omitted_and_explicit_identity_fields() {
    let harness = harness();
    let (status, created) = create(
        &harness,
        json!({
            "logicalId": "agent-attach",
            "metadata": { "team": "alpha" },
            "prompt_cache_options": { "ttl": "12h" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    // Omission keeps the stored identity: an attach without metadata or prompt
    // cache options is not a conflict (`design §4.1`).
    let (status, attached) = create(&harness, json!({ "logicalId": "agent-attach" })).await;
    assert_eq!(status, StatusCode::OK, "{attached}");
    assert_eq!(attached["created"], json!(false));
    assert_eq!(attached["metadata"], json!({ "team": "alpha" }));
    assert_eq!(attached["prompt_cache_options"]["ttl"], json!("12h"));

    // An explicit value equal to the stored one is accepted.
    let (status, same) = create(
        &harness,
        json!({
            "logicalId": "agent-attach",
            "metadata": { "team": "alpha" },
            "prompt_cache_options": { "ttl": "12h" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{same}");

    // A different explicit value conflicts: `{}` is the explicit default
    // `30m`, and `null` asserts "unset", against a stored `12h`.
    for body in [
        json!({ "logicalId": "agent-attach", "metadata": { "team": "beta" } }),
        json!({ "logicalId": "agent-attach", "metadata": {} }),
        json!({ "logicalId": "agent-attach", "metadata": null }),
        json!({ "logicalId": "agent-attach", "prompt_cache_options": {} }),
        json!({ "logicalId": "agent-attach", "prompt_cache_options": { "ttl": "30m" } }),
        json!({ "logicalId": "agent-attach", "prompt_cache_options": null }),
    ] {
        let (status, error) = create(&harness, body.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}: {error}");
        assert_eq!(error["error"]["code"], json!("identity_conflict"), "{body}");
    }

    // The refusals did not mutate the row.
    let (status, read) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-attach",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["metadata"], json!({ "team": "alpha" }));
    assert_eq!(read["prompt_cache_options"]["ttl"], json!("12h"));

    // `null` still matches a session with no prompt cache value; `{}` no longer
    // does, because it means the official default `30m`.
    let (status, _) = create(&harness, json!({ "logicalId": "agent-empty" })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, empty) = create(
        &harness,
        json!({
            "logicalId": "agent-empty",
            "metadata": {},
            "prompt_cache_options": null
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["metadata"], json!({}));
    assert_eq!(empty["prompt_cache_options"], json!(null));

    let (status, error) = create(
        &harness,
        json!({ "logicalId": "agent-empty", "prompt_cache_options": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], json!("identity_conflict"));

    // Two `{}` creates agree on the resolved default.
    let (status, _) = create(
        &harness,
        json!({ "logicalId": "agent-empty-default", "prompt_cache_options": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, same_default) = create(
        &harness,
        json!({ "logicalId": "agent-empty-default", "prompt_cache_options": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{same_default}");
    assert_eq!(
        same_default["prompt_cache_options"],
        json!({ "ttl": "30m" })
    );
}

#[tokio::test]
async fn update_and_delete_are_refused_while_a_turn_is_open() {
    let harness = harness_with(TurnPolicy::Manual);
    create(&harness, json!({ "logicalId": "agent-run" })).await;
    let (status, accepted) = submit(&harness, "agent-run", 1, "hello").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");

    let (status, error) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-run",
        Some(json!({ "metadata": { "a": "b" } })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], json!("turn_in_progress"));

    let (status, error) = send(
        &harness.router,
        Method::DELETE,
        "/v1/agents/sessions/agent-run",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], json!("turn_in_progress"));
}

// ── delete ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_cascades_rows_and_recreate_starts_clean() {
    let harness = harness();
    let (status, created) = create(&harness, json!({ "logicalId": "agent-del" })).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let first_root = created["rootSessionId"]
        .as_str()
        .expect("root id")
        .to_owned();
    let (status, _) = submit(&harness, "agent-del", 7, "first").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let first_turn = turn_id(7);
    assert!(harness
        .store
        .swarm_get_turn("agent-del", &first_turn)
        .await
        .expect("turn")
        .is_some());
    assert!(harness
        .store
        .swarm_get_input("agent-del", &input_id(7))
        .await
        .expect("input")
        .is_some());

    let (status, deleted) = send(
        &harness.router,
        Method::DELETE,
        "/v1/agents/sessions/agent-del",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted["id"], json!("agent-del"));
    assert_eq!(deleted["object"], json!("agent.session.deleted"));
    assert_eq!(deleted["deleted"], json!(true));

    for path in [
        "/v1/agents/sessions/agent-del",
        "/v1/agents/sessions/agent-del/items",
        "/v1/agents/sessions/agent-del/turns",
        "/v1/agents/sessions/agent-del/turns/turn_input_0192f0c5-0000-7000-8000-000000000007",
    ] {
        let (status, error) = send(&harness.router, Method::GET, path, None, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {error}");
        assert_eq!(error["error"]["code"], json!("unknown_agent"), "{path}");
    }
    let (status, error) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-del/events",
        Some(json!({ "inputId": input_id(8), "text": "after delete" })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");

    assert!(harness
        .store
        .swarm_get_session("agent-del")
        .await
        .expect("session")
        .is_none());
    assert_eq!(
        harness
            .store
            .swarm_last_seq("agent-del")
            .await
            .expect("seq"),
        0
    );
    assert!(harness
        .store
        .swarm_get_turn("agent-del", &first_turn)
        .await
        .expect("turn")
        .is_none());
    assert!(harness
        .store
        .swarm_get_input("agent-del", &input_id(7))
        .await
        .expect("input")
        .is_none());

    // Same logicalId, clean state: new root id, no items, no old turn.
    let (status, recreated) = create(&harness, json!({ "logicalId": "agent-del" })).await;
    assert_eq!(status, StatusCode::CREATED, "{recreated}");
    assert_eq!(recreated["created"], json!(true));
    let second_root = recreated["rootSessionId"]
        .as_str()
        .expect("root id")
        .to_owned();
    assert_ne!(first_root, second_root);

    let (status, items) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-del/items",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(items["items"].as_array().expect("items").is_empty());
    assert_eq!(items["lastItemSeq"], json!(0));

    let (status, turns) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-del/turns",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{turns}");
    assert!(turns["data"].as_array().expect("data").is_empty());
    assert_eq!(turns["has_more"], json!(false));

    let (status, error) = send(
        &harness.router,
        Method::GET,
        &format!("/v1/agents/sessions/agent-del/turns/{first_turn}"),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
}

// ── turns ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn turns_list_is_desc_by_default_and_after_is_exclusive() {
    let harness = harness();
    create(&harness, json!({ "logicalId": "agent-t" })).await;
    for index in 1..=3 {
        let (status, accepted) = submit(&harness, "agent-t", index, &format!("turn {index}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    }

    let (status, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-t/turns",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["object"], json!("list"));
    assert_eq!(page["has_more"], json!(false));
    assert_eq!(page["hasMore"], json!(false));
    let ids: Vec<&str> = page["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|turn| turn["id"].as_str().expect("id"))
        .collect();
    let expected_desc = vec![turn_id(3), turn_id(2), turn_id(1)];
    assert_eq!(ids, expected_desc, "default order is desc");
    for turn in page["data"].as_array().expect("data") {
        assert_eq!(turn["object"], json!("agent.session.turn"));
        assert_eq!(turn["agent_id"], json!("root"));
        assert_eq!(turn["session_id"], json!("agent-t"));
        assert_eq!(turn["status"], json!("completed"));
        assert_eq!(turn["usage"], json!(null));
        assert_eq!(turn["subagent_id"], json!(null));
        assert_eq!(turn["error"], json!(null));
        assert!(turn["created_at"].as_i64().is_some());
        assert!(turn["started_at"].as_i64().is_some());
        assert!(turn["completed_at"].as_i64().is_some());
    }

    let (status, asc) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-t/turns?order=asc",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let asc_ids: Vec<&str> = asc["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|turn| turn["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        asc_ids,
        vec![turn_id(1), turn_id(2), turn_id(3)],
        "asc follows creation order"
    );

    let (status, first_page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-t/turns?limit=2",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_page["data"].as_array().expect("data").len(), 2);
    assert_eq!(first_page["has_more"], json!(true));
    assert_eq!(first_page["nextCursor"], json!(turn_id(2)));

    let (status, second_page) = send(
        &harness.router,
        Method::GET,
        &format!(
            "/v1/agents/sessions/agent-t/turns?limit=2&after={}",
            turn_id(2)
        ),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_page["data"].as_array().expect("data").len(), 1);
    assert_eq!(second_page["data"][0]["id"], json!(turn_id(1)));
    assert_eq!(second_page["has_more"], json!(false));

    let (status, error) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-t/turns?after=turn_nope",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("invalid_query"));

    let (status, error) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-t/turns?cursor=abc",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("invalid_query"));
}

#[tokio::test]
async fn turn_retrieve_is_session_scoped() {
    let harness = harness();
    create(&harness, json!({ "logicalId": "agent-r1" })).await;
    create(&harness, json!({ "logicalId": "agent-r2" })).await;
    submit(&harness, "agent-r1", 5, "hello").await;

    let path = format!("/v1/agents/sessions/agent-r1/turns/{}", turn_id(5));
    let (status, turn) = send(&harness.router, Method::GET, &path, None, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{turn}");
    assert_eq!(turn["id"], json!(turn_id(5)));
    assert_eq!(turn["session_id"], json!("agent-r1"));
    assert_eq!(turn["status"], json!("completed"));

    let (status, error) = send(
        &harness.router,
        Method::GET,
        &format!("/v1/agents/sessions/agent-r2/turns/{}", turn_id(5)),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("unknown_agent"));
    assert_eq!(error["error"]["details"]["turnId"], json!(turn_id(5)));

    // The official per-turn item list is not mappable: items have no turn id.
    let (status, error) = send(
        &harness.router,
        Method::GET,
        &format!("/v1/agents/sessions/agent-r1/turns/{}/items", turn_id(5)),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], json!("unsupported_contract"));
    assert!(error["error"]["message"]
        .as_str()
        .expect("message")
        .contains("sessions.turns.items"));
}

#[tokio::test]
async fn a_failed_turn_persists_its_customer_safe_error() {
    struct FailingAppendStore {
        inner: Arc<MemorySwarmStore>,
    }

    #[async_trait]
    impl SwarmStore for FailingAppendStore {
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
        async fn swarm_patch_session(&self, patch: SwarmSessionPatch) -> Result<(), StoreError> {
            self.inner.swarm_patch_session(patch).await
        }
        async fn swarm_list_sessions(
            &self,
            window: &SessionsWindow,
        ) -> Result<Vec<SwarmSessionRow>, StoreError> {
            self.inner.swarm_list_sessions(window).await
        }
        async fn swarm_delete_session(&self, logical_id: &str) -> Result<(), StoreError> {
            self.inner.swarm_delete_session(logical_id).await
        }
        async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
            // Acceptance records the user item; only the turn's assistant
            // output fails, so the failure happens inside `run_turn`.
            if row.payload.get("role").and_then(Value::as_str) == Some("assistant") {
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
        async fn swarm_item_page(
            &self,
            logical_id: &str,
            order: ItemOrder,
            after: Option<u64>,
            limit: u32,
        ) -> Result<SwarmItemPage, StoreError> {
            self.inner
                .swarm_item_page(logical_id, order, after, limit)
                .await
        }
        async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError> {
            self.inner.swarm_last_seq(logical_id).await
        }
        async fn swarm_get_item(
            &self,
            logical_id: &str,
            item_id: &str,
        ) -> Result<Option<SwarmItemRow>, StoreError> {
            self.inner.swarm_get_item(logical_id, item_id).await
        }
        async fn swarm_put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError> {
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
        async fn swarm_upsert_turn(&self, row: SwarmTurnRow) -> Result<(), StoreError> {
            self.inner.swarm_upsert_turn(row).await
        }
        async fn swarm_get_turn(
            &self,
            logical_id: &str,
            turn_id: &str,
        ) -> Result<Option<SwarmTurnRow>, StoreError> {
            self.inner.swarm_get_turn(logical_id, turn_id).await
        }
        async fn swarm_turn_page(
            &self,
            logical_id: &str,
            window: &TurnsWindow,
        ) -> Result<Vec<SwarmTurnRow>, StoreError> {
            self.inner.swarm_turn_page(logical_id, window).await
        }
        async fn swarm_close_open_turns(
            &self,
            logical_id: &str,
            status: TurnStatus,
            completed_at: i64,
            error: Option<Value>,
        ) -> Result<u64, StoreError> {
            self.inner
                .swarm_close_open_turns(logical_id, status, completed_at, error)
                .await
        }
        async fn swarm_has_open_turn(&self, logical_id: &str) -> Result<bool, StoreError> {
            self.inner.swarm_has_open_turn(logical_id).await
        }
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
    ]);
    let config = SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config");
    let inner = Arc::new(MemorySwarmStore::new());
    let store = Arc::new(FailingAppendStore {
        inner: Arc::clone(&inner),
    });
    let service = Arc::new(InMemorySwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ServiceConfig::default(),
    ));
    let router = build_router(AppState::new(service, Arc::new(config)));
    let harness = Harness {
        router,
        _workspace: workspace,
        store: Arc::clone(&inner),
    };

    create(&harness, json!({ "logicalId": "agent-f" })).await;
    let (status, error) = submit(&harness, "agent-f", 9, "fail this turn").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], json!("upstream_unavailable"));
    assert!(
        harness
            .store
            .swarm_get_turn("agent-f", &turn_id(9))
            .await
            .expect("turn")
            .is_some(),
        "the failed turn row must exist"
    );

    let (status, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-f/turns",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let turns = page["data"].as_array().expect("data");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["status"], json!("failed"));
    assert_eq!(turns[0]["error"]["code"], json!("internal_error"));
    assert!(turns[0]["error"]["message"]
        .as_str()
        .is_some_and(|message| !message.is_empty()));
    assert!(turns[0]["completed_at"].as_i64().is_some());

    let (status, turn) = send(
        &harness.router,
        Method::GET,
        &format!("/v1/agents/sessions/agent-f/turns/{}", turn_id(9)),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(turn["error"], turns[0]["error"]);
}

// ── explicit 404 for unmappable official resources ─────────────────────────

#[tokio::test]
async fn unmappable_official_resources_return_gateway_json_404() {
    let harness = harness();
    let cases = [
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/traces",
            "sessions.traces",
        ),
        (
            Method::POST,
            "/v1/agents/sessions/agent-x/traces",
            "sessions.traces",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/artifacts",
            "sessions.artifacts",
        ),
        (
            Method::POST,
            "/v1/agents/sessions/agent-x/artifacts",
            "sessions.artifacts",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/artifacts/art_1",
            "sessions.artifacts",
        ),
        (
            Method::DELETE,
            "/v1/agents/sessions/agent-x/artifacts/art_1",
            "sessions.artifacts",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/artifacts/art_1/content",
            "sessions.artifacts.content",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/subagents",
            "sessions.subagents",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/subagents/sub_1",
            "sessions.subagents",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/subagents/sub_1/items",
            "sessions.subagents.items",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/subagents/sub_1/turns",
            "sessions.subagents.turns",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/subagents/sub_1/turns/turn_1",
            "sessions.subagents.turns",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/subagents/sub_1/turns/turn_1/items",
            "sessions.subagents.turns.items",
        ),
        (
            Method::GET,
            "/v1/agents/sessions/agent-x/turns/turn_1/items",
            "sessions.turns.items",
        ),
    ];
    for (method, path, resource) in cases {
        let body = matches!(method, Method::POST).then(|| json!({}));
        let (status, error) = send(&harness.router, method.clone(), path, body, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}: {error}");
        assert_eq!(
            error["error"]["code"],
            json!("unsupported_contract"),
            "{method} {path}"
        );
        assert_eq!(error["error"]["details"]["resource"], json!(resource));
        assert!(error["error"]["message"]
            .as_str()
            .expect("message")
            .contains(resource));

        let (status, _) = send(&harness.router, method, path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "the Bearer gate must run before the explicit 404: {path}"
        );
    }
}
