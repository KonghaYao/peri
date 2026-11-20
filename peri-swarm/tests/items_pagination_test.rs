use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::service::{InMemorySwarmService, ServiceConfig};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-deployment-key";

struct Harness {
    router: Router,
    _workspace: TempDir,
}

fn harness() -> Harness {
    let workspace = tempfile::tempdir().expect("workspace");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
    ]);
    let config = SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config");
    let service = Arc::new(InMemorySwarmService::in_memory(ServiceConfig::default()));
    let router = build_router(AppState::new(service, Arc::new(config)));
    Harness {
        router,
        _workspace: workspace,
    }
}

async fn send(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let body = match body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).expect("request"))
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

async fn seed_five_items(harness: &Harness) {
    let (status, _, value) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    for index in 1..=5 {
        let (status, _, value) = send(
            &harness.router,
            Method::POST,
            "/v1/agents/sessions/agent-01/events",
            Some(json!({
                "inputId": uuid::Uuid::now_v7().to_string(),
                "text": format!("message {index}")
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    }
}

fn seqs(value: &Value) -> Vec<u64> {
    value["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["seq"].as_u64().expect("seq"))
        .collect()
}

#[tokio::test]
async fn ascending_pages_cover_every_item_once() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (status, _, first) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["object"], json!("list"));
    assert_eq!(first["logicalId"], json!("agent-01"));
    assert_eq!(seqs(&first), [1, 2]);
    assert_eq!(first["nextCursor"], json!(2));
    assert_eq!(first["hasMore"], json!(true));
    assert_eq!(first["lastItemSeq"], json!(5));
    assert_eq!(first["order"], json!("asc"));

    let (_, _, second) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&after=2",
        None,
    )
    .await;
    assert_eq!(seqs(&second), [3, 4]);
    assert_eq!(second["nextCursor"], json!(4));

    let (_, _, third) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&afterSeq=4",
        None,
    )
    .await;
    assert_eq!(seqs(&third), [5]);
    assert_eq!(third["nextCursor"], json!(null));
    assert_eq!(third["hasMore"], json!(false));
    assert_eq!(third["lastItemSeq"], json!(5));

    let mut collected = Vec::new();
    let mut cursor: Option<u64> = None;
    loop {
        let uri = match cursor {
            Some(cursor) => format!("/v1/agents/sessions/agent-01/items?limit=2&afterSeq={cursor}"),
            None => "/v1/agents/sessions/agent-01/items?limit=2".to_owned(),
        };
        let (_, _, page) = send(&harness.router, Method::GET, &uri, None).await;
        collected.extend(seqs(&page));
        match page["nextCursor"].as_u64() {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(collected, [1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn descending_pages_walk_backwards_with_a_stable_cursor() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (status, _, newest) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=desc",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{newest}");
    assert_eq!(seqs(&newest), [5, 4]);
    assert_eq!(newest["nextCursor"], json!(4));
    assert_eq!(newest["hasMore"], json!(true));
    assert_eq!(newest["order"], json!("desc"));

    let (_, _, older) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=desc&after=4",
        None,
    )
    .await;
    assert_eq!(seqs(&older), [3, 2]);

    let (_, _, oldest) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=2&order=desc&after=2",
        None,
    )
    .await;
    assert_eq!(seqs(&oldest), [1]);
    assert_eq!(oldest["nextCursor"], json!(null));
    assert_eq!(oldest["hasMore"], json!(false));
}

#[tokio::test]
async fn item_shape_carries_stable_identity() {
    let harness = harness();
    seed_five_items(&harness).await;
    let (_, _, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?limit=1",
        None,
    )
    .await;
    let item = &page["items"][0];
    assert_eq!(item["seq"], json!(1));
    assert!(item["itemId"]
        .as_str()
        .expect("item id")
        .starts_with("itim_"));
    assert_eq!(item["kind"], json!("message"));
    assert_eq!(item["source"]["agentId"], json!("root"));
    assert_eq!(item["source"]["taskId"], json!(null));
    assert_eq!(item["payload"]["text"], json!("fake-echo: message 1"));
    let created_at = item["createdAt"].as_str().expect("createdAt");
    assert!(created_at.contains('T'), "{created_at}");
    assert!(
        created_at.ends_with('Z') || created_at.ends_with("+00:00"),
        "createdAt must be UTC RFC3339: {created_at}"
    );
    assert_eq!(item["truncated"], json!(false));
}

#[tokio::test]
async fn after_aliases_must_agree_and_limits_are_bounded() {
    let harness = harness();
    seed_five_items(&harness).await;

    let (_, _, agreed) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?after=2&afterSeq=2",
        None,
    )
    .await;
    assert_eq!(seqs(&agreed), [3, 4, 5]);

    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?after=2&afterSeq=3",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("conflicting_cursor"));

    for query in [
        "limit=0",
        "limit=501",
        "limit=abc",
        "order=sideways",
        "bogus=1",
    ] {
        let uri = format!("/v1/agents/sessions/agent-01/items?{query}");
        let (status, _, value) = send(&harness.router, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(value["error"]["code"], json!("invalid_query"), "{query}");
    }
}

#[tokio::test]
async fn cursor_beyond_the_tail_is_an_empty_page() {
    let harness = harness();
    seed_five_items(&harness).await;
    let (status, _, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?afterSeq=99",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), Vec::<u64>::new());
    assert_eq!(page["nextCursor"], json!(null));
    assert_eq!(page["hasMore"], json!(false));
    assert_eq!(page["lastItemSeq"], json!(5));
}

#[tokio::test]
async fn unknown_agent_items_is_404() {
    let harness = harness();
    let (status, _, value) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/missing/items",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"]["code"], json!("unknown_agent"));
}
