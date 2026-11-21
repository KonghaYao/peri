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

/// Like [`read_until`], but also waits for the frame containing `needle` to be
/// terminated, so the returned text can be parsed into complete frames.
async fn read_complete(body: Body, needle: &str) -> String {
    let mut stream = body.into_data_stream();
    let mut collected = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(collected.contains(needle) && collected.ends_with("\n\n")) {
        let next = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("sse timeout")
            .expect("sse ended early")
            .expect("sse chunk");
        collected.push_str(&String::from_utf8_lossy(&next));
    }
    collected
}

/// Decode `(event name, data)` pairs from the frames of one SSE body.
fn frames(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut event = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(name) = line.strip_prefix("event: ") {
                    event = Some(name.to_owned());
                } else if let Some(payload) = line.strip_prefix("data: ") {
                    data.push_str(payload);
                }
            }
            let value = serde_json::from_str(&data).ok()?;
            Some((event?, value))
        })
        .collect()
}

/// The official `agent.session.*` frames as `(type, event_id)` pairs, in order.
fn official_ids(text: &str) -> Vec<(String, String)> {
    frames(text)
        .into_iter()
        .filter(|(event, _)| event.starts_with("agent.session."))
        .map(|(event, value)| {
            let event_id = value["event_id"].as_str().expect("event_id").to_owned();
            assert_eq!(
                value["type"],
                json!(event),
                "the SSE name and the JSON type agree"
            );
            (event, event_id)
        })
        .collect()
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

    // The response returns before the first turn runs; wait for the turn's
    // final official frame so every first-turn frame is in the buffer.
    let text = read_complete(response.into_body(), "event: agent.session.idle").await;
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

    // The official compatibility frames of the first turn arrive on the same
    // subscription, in order, with SSE name == JSON type.
    let official: Vec<(String, Value)> = frames(&text)
        .into_iter()
        .filter(|(event, _)| event.starts_with("agent.session."))
        .collect();
    let names: Vec<&str> = official.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(
        names,
        [
            "agent.session.turn.created",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{text}"
    );
    for (event, value) in &official {
        assert_eq!(value["type"], json!(event), "SSE name and JSON type agree");
    }
    assert_eq!(official[1].1["item"]["role"], json!("user"), "{text}");
    assert_eq!(official[2].1["item"]["role"], json!("assistant"), "{text}");
    assert_eq!(
        official[0].1["turn_id"], official[1].1["turn_id"],
        "the user item belongs to the created turn"
    );
    assert_eq!(
        official[1].1["turn_id"], official[2].1["turn_id"],
        "the assistant item belongs to the same turn"
    );
    assert!(
        !text.contains("event: gap"),
        "the first turn must not lag: {text}"
    );

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
async fn include_obfuscation_is_accepted_and_ignored() {
    let harness = harness(60_000);
    create_session(&harness).await;
    submit(&harness, "first").await;

    let baseline = {
        let response = harness
            .router
            .clone()
            .oneshot(request(
                Method::GET,
                "/v1/agents/sessions/agent-01/events?afterSeq=0",
                None,
                Some(TOKEN),
                &[],
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        read_complete(response.into_body(), "event: agent.session.idle").await
    };
    let baseline_frames = frames(&baseline);
    assert!(!baseline_frames.is_empty(), "{baseline}");

    for flag in ["false", "true"] {
        let uri =
            format!("/v1/agents/sessions/agent-01/events?afterSeq=0&include_obfuscation={flag}");
        let response = harness
            .router
            .clone()
            .oneshot(request(Method::GET, &uri, None, Some(TOKEN), &[]))
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "official `include_obfuscation={flag}` must not be rejected"
        );
        let text = read_complete(response.into_body(), "event: agent.session.idle").await;
        assert_eq!(
            frames(&text),
            baseline_frames,
            "the gateway emits no padding, so `{flag}` changes nothing: {text}"
        );
    }
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
/// A subscription without a cursor starts at the tail (live only), so the new
/// user frame, the assistant frame and the turn terminal arrive without
/// replaying older items.
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
            "/v1/agents/sessions/agent-01/events",
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
            "/v1/agents/sessions/agent-01/events?afterSeq=0",
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

#[tokio::test]
async fn official_frames_stream_in_sdk_order_and_replay_identically() {
    let harness = harness(60_000);
    create_session(&harness).await;

    // Subscribe before the turn so this observes live delivery.
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
    let live_body = response.into_body();
    submit(&harness, "hello").await;
    let live = read_complete(live_body, "event: agent.session.idle").await;

    // The order the official SDK's `TurnState` relies on.
    let created = live
        .find("event: agent.session.turn.created")
        .expect("turn created");
    let user_added = live
        .find("\"event_id\":\"evt_item_1_added\"")
        .expect("user item added");
    let assistant_added = live
        .find("\"event_id\":\"evt_item_2_added\"")
        .expect("assistant item added");
    let completed = live
        .find("event: agent.session.turn.completed")
        .expect("turn completed");
    let idle = live
        .find("event: agent.session.idle")
        .expect("session idle");
    assert!(created < user_added, "{live}");
    assert!(user_added < assistant_added, "{live}");
    assert!(assistant_added < completed, "{live}");
    assert!(completed < idle, "{live}");

    let live_frames = frames(&live);
    let turn_completed_at = live_frames
        .iter()
        .position(|(event, _)| event == "agent.session.turn.completed")
        .expect("turn completed frame");
    assert_eq!(
        live_frames[turn_completed_at + 1].0,
        "agent.session.idle",
        "`agent.session.idle` immediately follows `turn.completed`: {live}"
    );
    let idle_event = &live_frames[turn_completed_at + 1].1;
    assert_eq!(idle_event["session"]["status"], json!("idle"));
    let created_event = &live_frames
        .iter()
        .find(|(event, _)| event == "agent.session.turn.created")
        .expect("turn created frame")
        .1;
    let turn_id = created_event["turn_id"].as_str().expect("turn id");
    assert!(turn_id.starts_with("turn_input_"), "{turn_id}");
    assert!(!turn_id.starts_with("msg_"), "a messageId is not a turn id");
    assert_eq!(created_event["turn"]["id"], json!(turn_id));
    assert_eq!(created_event["turn"]["status"], json!("in_progress"));
    assert_eq!(created_event["turn"]["subagent_id"], json!(null));

    // The backlog is derived from the persisted rows only, so the same frames
    // (and the same `event_id`s) come back on a fresh subscription. `afterSeq=0`
    // is the explicit "replay everything" cursor; a subscription without any
    // cursor is live-only by design.
    let response = harness
        .router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/agents/sessions/agent-01/events?afterSeq=0",
            None,
            Some(TOKEN),
            &[],
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let replayed = read_complete(response.into_body(), "event: agent.session.idle").await;
    assert_eq!(
        official_ids(&live),
        official_ids(&replayed),
        "live and replay must carry the same official identities"
    );
    assert_eq!(
        official_ids(&live).len(),
        5,
        "created, 2 items, completed, idle"
    );
}

/// The official `sessions.stream` subscribes **without** a cursor and then
/// submits the input. Replaying the previous turn would put its `idle` in the
/// new `TurnState`; the subscription must start at the tail.
#[tokio::test]
async fn subscription_without_cursor_receives_live_events_only() {
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
    assert!(!text.contains("event: item"), "history replayed: {text}");
    assert!(
        !text.contains("agent.session.turn.created"),
        "the old turn was replayed: {text}"
    );
    assert!(
        !text.contains("agent.session.idle"),
        "the old idle would end the official stream: {text}"
    );
    assert!(text.contains("\"lastItemSeq\":2"), "{text}");

    // The stream is still open and the next turn arrives live.
    submit(&harness, "second").await;
    text.clear();
    while !(text.contains("event: agent.session.idle") && text.ends_with("\n\n")) {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("turn timeout")
            .expect("sse ended early")
            .expect("sse chunk");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(text.contains("agent.session.turn.created"), "{text}");
    assert!(text.contains("\"seq\":3"), "{text}");
    assert!(text.contains("\"seq\":4"), "{text}");
    assert!(text.contains("agent.session.turn.completed"), "{text}");
}

/// An explicit cursor equal to the tail replays no rows. When the session is
/// idle the last turn is known to be closed, so its (unpersisted) terminal is
/// republished instead of leaving a reconnecting client without one.
#[tokio::test]
async fn cursor_at_the_tail_republishes_the_last_terminal() {
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
    let text = read_complete(response.into_body(), "event: agent.session.idle").await;
    assert!(!text.contains("event: item"), "no rows to replay: {text}");
    assert!(
        !text.contains("agent.session.turn.created"),
        "the turn was already created before the cursor: {text}"
    );
    let replayed = frames(&text);
    assert_eq!(
        replayed
            .iter()
            .map(|(event, _)| event.as_str())
            .collect::<Vec<_>>(),
        ["agent.session.turn.completed", "agent.session.idle"],
        "{text}"
    );
    assert_eq!(
        replayed[0].1["event_id"],
        json!(replayed[1].1["event_id"]
            .as_str()
            .expect("idle id")
            .replace("_idle", "_completed")),
        "the terminal keeps the derived turn identity: {text}"
    );
}
