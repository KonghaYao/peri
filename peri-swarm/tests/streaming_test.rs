use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use futures::StreamExt;
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, SSE_HEARTBEAT_MS_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::service::{InMemorySwarmService, ServiceCall, ServiceConfig};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-deployment-key";

struct Harness {
    router: Router,
    service: Arc<InMemorySwarmService>,
    _workspace: TempDir,
}

fn harness(heartbeat_ms: u64) -> Harness {
    let workspace = tempfile::tempdir().expect("workspace");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
        (SSE_HEARTBEAT_MS_ENV.to_owned(), heartbeat_ms.to_string()),
    ]);
    let config = SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("config");
    let service = Arc::new(InMemorySwarmService::in_memory(ServiceConfig::default()));
    let router = build_router(AppState::new(service.clone(), Arc::new(config)));
    Harness {
        router,
        service,
        _workspace: workspace,
    }
}

fn request(
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
    extra: &[(&str, &str)],
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    };
    builder.body(body).expect("request")
}

async fn json_call(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request(method, uri, body, Some(TOKEN), headers))
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn create_session(harness: &Harness) {
    let (status, value) = json_call(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-01" })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
}

async fn submit(harness: &Harness, text: &str) {
    let (status, value) = json_call(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions/agent-01/events",
        Some(json!({
            "inputId": uuid::Uuid::now_v7().to_string(),
            "text": text
        })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
}

/// Read SSE chunks until `needle` appears, with a hard deadline.
async fn read_until(body: Body, needle: &str) -> String {
    let mut stream = body.into_data_stream();
    let mut collected = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !collected.contains(needle) {
        let next = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("sse timeout")
            .expect("sse ended early")
            .expect("sse chunk");
        collected.push_str(&String::from_utf8_lossy(&next));
    }
    collected
}

#[tokio::test]
async fn stream_create_subscribes_before_the_first_turn() {
    let harness = harness(60_000);
    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/agents/sessions",
            Some(json!({
                "logicalId": "agent-01",
                "input": "hello",
                "stream": true
            })),
            Some(TOKEN),
            &[("openai-beta", "agents=v1")],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert!(headers
        .get("content-type")
        .expect("content-type")
        .to_str()
        .expect("ascii")
        .starts_with("text/event-stream"));
    assert_eq!(
        headers
            .get("cache-control")
            .expect("cache")
            .to_str()
            .expect("ascii"),
        "no-cache"
    );
    assert_eq!(
        headers
            .get("x-accel-buffering")
            .expect("buffering")
            .to_str()
            .expect("ascii"),
        "no"
    );
    assert_eq!(
        headers
            .get("x-peri-swarm-contract")
            .expect("contract")
            .to_str()
            .expect("ascii"),
        "1"
    );

    let text = read_until(response.into_body(), "\"stopReason\":\"end_turn\"").await;
    assert!(text.contains("event: session"), "{text}");
    let session_at = text.find("event: session").expect("session event");
    // The create input is logged before the turn opens, so the subscription
    // (opened before the turn) sees the user frame first.
    let user_at = text.find("\"role\":\"user\"").expect("user item");
    let started_at = text.find("\"state\":\"started\"").expect("started");
    let item_at = text.find("\"seq\":2").expect("assistant item");
    let completed_at = text.find("\"state\":\"completed\"").expect("completed");
    assert!(session_at < user_at, "{text}");
    assert!(user_at < started_at, "{text}");
    assert!(started_at < item_at, "{text}");
    assert!(item_at < completed_at, "{text}");
    assert!(text.contains("id: 1"), "{text}");
    assert!(text.contains("\"seq\":1"), "{text}");

    assert_eq!(
        harness.service.calls(),
        vec![
            ServiceCall::OpenStream("agent-01".to_owned()),
            ServiceCall::CreateSession("agent-01".to_owned()),
        ]
    );
}

#[tokio::test]
async fn events_stream_replays_after_the_cursor() {
    let harness = harness(60_000);
    create_session(&harness).await;
    submit(&harness, "first").await;
    submit(&harness, "second").await;

    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events?afterSeq=1",
            None,
            Some(TOKEN),
            &[],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let text = read_until(response.into_body(), "\"seq\":2").await;
    assert!(text.contains("id: 2"), "{text}");
    assert!(text.contains("event: item"), "{text}");
    assert!(!text.contains("\"seq\":1"), "{text}");
}

#[tokio::test]
async fn last_event_id_header_selects_the_cursor() {
    let harness = harness(60_000);
    create_session(&harness).await;
    submit(&harness, "first").await;
    submit(&harness, "second").await;

    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events",
            None,
            Some(TOKEN),
            &[("last-event-id", "1")],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let text = read_until(response.into_body(), "\"seq\":2").await;
    assert!(!text.contains("\"seq\":1"), "{text}");
}

#[tokio::test]
async fn conflicting_and_invalid_cursors_are_rejected_before_streaming() {
    let harness = harness(60_000);
    create_session(&harness).await;

    let (status, value) = json_call(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/events?afterSeq=1",
        None,
        &[("last-event-id", "2")],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("conflicting_cursor"));

    let (status, value) = json_call(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/events",
        None,
        &[("last-event-id", "not-a-number")],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("invalid_query"));

    let (status, value) = json_call(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/events?afterSeq=1&after=2",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], json!("conflicting_cursor"));
}

/// The README's realtime order: open the stream at the current tail, then POST.
/// The stream must deliver the new user frame, the assistant frame and the turn
/// terminal without replaying older items.
#[tokio::test]
async fn stream_opened_before_post_delivers_the_new_turn() {
    let harness = harness(60_000);
    create_session(&harness).await;
    submit(&harness, "first").await;

    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events?afterSeq=2",
            None,
            Some(TOKEN),
            &[],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    // POST runs the turn while the body is not polled; the transport buffers
    // the new frames, which is what the SDK example relies on.
    submit(&harness, "second").await;

    let text = read_until(response.into_body(), "\"state\":\"completed\"").await;
    assert!(!text.contains("\"seq\":1"), "{text}");
    assert!(!text.contains("\"seq\":2"), "{text}");
    assert!(text.contains("\"seq\":3"), "{text}");
    assert!(text.contains("\"role\":\"user\""), "{text}");
    assert!(text.contains("\"text\":\"second\""), "{text}");
    assert!(text.contains("\"seq\":4"), "{text}");
    assert!(text.contains("\"state\":\"started\""), "{text}");
}

#[tokio::test]
async fn heartbeat_has_no_id_and_reports_the_last_item_seq() {
    let harness = harness(25);
    create_session(&harness).await;
    submit(&harness, "first").await;

    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events",
            None,
            Some(TOKEN),
            &[],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let text = read_until(response.into_body(), "event: heartbeat").await;

    let item_at = text.find("event: item").expect("item replay");
    let heartbeat_at = text.find("event: heartbeat").expect("heartbeat");
    assert!(item_at < heartbeat_at, "{text}");
    let heartbeat_frame = &text[heartbeat_at..];
    assert!(!heartbeat_frame.contains("id:"), "{text}");
    assert!(heartbeat_frame.contains("\"lastItemSeq\":2"), "{text}");
}

/// A stream opened at the tail has nothing to replay. Its heartbeat must still
/// report the session's real progress, not a constant 0, and must keep doing so
/// while the session advances. The stream itself stays open by design.
#[tokio::test]
async fn heartbeat_reports_the_session_tail_while_the_stream_stays_open() {
    let harness = harness(25);
    create_session(&harness).await;
    submit(&harness, "first").await;

    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events?afterSeq=2",
            None,
            Some(TOKEN),
            &[],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut text = String::new();
    while !text.contains("event: heartbeat") {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("heartbeat timeout")
            .expect("sse ended early")
            .expect("sse chunk");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(!text.contains("event: item"), "nothing to replay: {text}");
    assert!(text.contains("\"lastItemSeq\":2"), "{text}");

    // The stream is not closed by `after == tail`; advance the session and
    // check a later heartbeat reflects the new tail (user 3 + reply 4).
    submit(&harness, "second").await;
    text.clear();
    while !text.contains("\"lastItemSeq\":4") {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("heartbeat timeout after the new turn")
            .expect("sse ended early")
            .expect("sse chunk");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
}

#[tokio::test]
async fn empty_session_stream_emits_a_heartbeat() {
    let harness = harness(25);
    create_session(&harness).await;
    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events",
            None,
            Some(TOKEN),
            &[],
        ))
        .await
        .expect("response");
    let text = read_until(response.into_body(), "event: heartbeat").await;
    assert!(text.contains("\"lastItemSeq\":0"), "{text}");
    assert!(!text.contains("id:"), "{text}");
}

#[tokio::test]
async fn streaming_requires_authentication() {
    let harness = harness(60_000);
    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events",
            None,
            None,
            &[],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("x-peri-swarm-contract")
            .expect("contract")
            .to_str()
            .expect("ascii"),
        "1"
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["error"]["code"], json!("unauthorized"));
}

#[tokio::test]
async fn stream_create_failure_returns_json_instead_of_sse() {
    let harness = harness(60_000);
    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/agents/sessions",
            Some(json!({
                "logicalId": "agent-01",
                "stream": true,
                "environment": { "type": "container" }
            })),
            Some(TOKEN),
            &[],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["error"]["code"], json!("unsupported_environment"));
}

#[tokio::test]
async fn unknown_agent_subscribe_returns_404_json() {
    let harness = harness(60_000);
    let (status, value) = json_call(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/missing/events",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"]["code"], json!("unknown_agent"));
}
