//! End-to-end OTLP/HTTP check against a local stub collector.
//!
//! One serial test owns this module's process-environment touch: it clears any
//! ambient `OTEL_*` variables so the exporter builders cannot be redirected by
//! the developer/CI environment, starts a loopback stub, and drives the real
//! gateway router. Pure configuration parsing lives in unit tests and never
//! mutates the environment.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::response::Response;
use axum::Router;
use futures::StreamExt;
use peri_swarm::config::{SwarmConfig, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::http::{build_router, AppState};
use peri_swarm::service::{InMemorySwarmService, ServiceConfig};
use peri_swarm::telemetry::{SignalConfig, TelemetryConfig, TelemetryGuard};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "telemetry-test-deployment-key";
const AUTH_CANARY: &str = "AUTH-CANARY-7f31";
const COOKIE_CANARY: &str = "COOKIE-CANARY-2a90";
const BODY_CANARY: &str = "BODY-CANARY-e8c4";
const QUERY_CANARY: &str = "QUERY-CANARY-91bd";

#[derive(Clone, Debug)]
struct StubRequest {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Default)]
struct StubState {
    requests: Mutex<Vec<StubRequest>>,
}

async fn stub_handler(
    State(state): State<Arc<StubState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let headers = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or("<non-ascii>").to_owned(),
            )
        })
        .collect();
    state.requests.lock().expect("stub lock").push(StubRequest {
        path: format!("{} {}", method.as_str(), uri.path()),
        headers,
        body: body.to_vec(),
    });
    StatusCode::OK
}

async fn start_stub() -> (String, Arc<StubState>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(StubState::default());
    let app = Router::new()
        .fallback(stub_handler)
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub bind");
    let address = listener.local_addr().expect("stub address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub serve");
    });
    (format!("http://{address}"), state, task)
}

fn export_config(base: &str) -> TelemetryConfig {
    let headers = vec![("x-otlp-test".to_owned(), "present".to_owned())];
    TelemetryConfig {
        hostname: Some("telemetry-test-host".to_owned()),
        pid: 4242,
        traces: SignalConfig {
            endpoint: Some(format!("{base}/v1/traces")),
            headers: headers.clone(),
        },
        metrics: SignalConfig {
            endpoint: Some(format!("{base}/v1/metrics")),
            headers: headers.clone(),
        },
        logs: SignalConfig {
            endpoint: Some(format!("{base}/v1/logs")),
            headers,
        },
        trace_batch_delay: Duration::from_millis(50),
        log_batch_delay: Duration::from_millis(50),
        metric_export_interval: Duration::from_millis(100),
        ..TelemetryConfig::default()
    }
}

fn harness(telemetry: Option<peri_swarm::telemetry::HttpTelemetry>) -> (Router, TempDir) {
    let workspace = tempfile::tempdir().expect("workspace");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TOKEN.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
    ]);
    let config = SwarmConfig::from_env_map(|name| env.get(name).cloned()).expect("swarm config");
    let service = Arc::new(InMemorySwarmService::in_memory(ServiceConfig::default()));
    let state = AppState::new(service, Arc::new(config));
    let state = match telemetry {
        Some(telemetry) => state.with_telemetry(telemetry),
        None => state,
    };
    (build_router(state), workspace)
}

fn request(
    method: Method,
    uri: &str,
    body: Option<Value>,
    auth: Option<&str>,
    headers: &[(&str, &str)],
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(auth) = auth {
        builder = builder.header("authorization", auth);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("request")
}

async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    auth: Option<&str>,
    headers: &[(&str, &str)],
) -> Response {
    router
        .clone()
        .oneshot(request(method, uri, body, auth, headers))
        .await
        .expect("response")
}

async fn read_until_sse_frame(body: Body) -> String {
    let mut stream = body.into_data_stream();
    let mut collected = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !collected.contains("event: session") {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("sse timeout")
            .expect("sse ended early")
            .expect("sse chunk");
        collected.push_str(&String::from_utf8_lossy(&chunk));
    }
    collected
}

fn payloads(requests: &[StubRequest]) -> Vec<(String, String)> {
    requests
        .iter()
        .map(|request| {
            (
                request.path.clone(),
                String::from_utf8_lossy(&request.body).into_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn otlp_exports_three_signals_without_leaking_requests() {
    // Exporter builders read the process environment themselves; keep this test
    // deterministic. No other test in the binary reads OTEL_*, so this does not
    // race with the rest of the suite.
    let saved_env: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| name.starts_with("OTEL_") || name == "NO_PROXY")
        .collect();
    for (name, _) in &saved_env {
        std::env::remove_var(name);
    }
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");

    let (base, stub, server) = start_stub().await;

    // Disabled/no-endpoint mode: nothing may reach the stub.
    {
        let guard = TelemetryGuard::start(TelemetryConfig::default());
        assert!(
            guard.http_telemetry().is_none(),
            "no endpoint must build no HTTP telemetry"
        );
        guard.force_flush();
        let disabled = TelemetryGuard::start(TelemetryConfig {
            disabled: true,
            ..export_config(&base)
        });
        assert!(
            disabled.http_telemetry().is_none(),
            "OTEL_SDK_DISABLED must ignore even configured endpoints"
        );
        disabled.force_flush();
        let (router, _workspace) = harness(None);
        let response = call(&router, Method::GET, "/healthz", None, None, &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            stub.requests.lock().expect("stub lock").is_empty(),
            "an unconfigured gateway must not connect to a collector"
        );
    }

    let guard = TelemetryGuard::start(export_config(&base));
    let http = guard.http_telemetry().unwrap_or_else(|| {
        panic!(
            "configured telemetry must expose a handle; warnings={:?}",
            guard.warnings()
        )
    });
    let (router, _workspace) = harness(Some(http));

    // Health probe: skipped entirely.
    assert_eq!(
        call(&router, Method::GET, "/healthz", None, None, &[])
            .await
            .status(),
        StatusCode::OK
    );

    // Authenticated create with a canary in the body (metadata is persisted but
    // must never appear in telemetry).
    let created = call(
        &router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({
            "logicalId": "agent-meta",
            "metadata": { "note": BODY_CANARY },
        })),
        Some(&format!("Bearer {TOKEN}")),
        &[],
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    // A regular read carries the session id in the matched path.
    let read = call(
        &router,
        Method::GET,
        "/v1/agents/sessions/agent-meta",
        None,
        Some(&format!("Bearer {TOKEN}")),
        &[],
    )
    .await;
    assert_eq!(read.status(), StatusCode::OK);

    // The streaming route: the middleware must return the original SSE body.
    let events = call(
        &router,
        Method::GET,
        "/v1/agents/sessions/agent-meta/events",
        None,
        Some(&format!("Bearer {TOKEN}")),
        &[],
    )
    .await;
    assert_eq!(events.status(), StatusCode::OK);
    let headers = events.headers();
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
            .unwrap(),
        "no-cache"
    );
    drop(events.into_body());

    // SSE on a non-`/events` route: read the initial frame to prove the body was
    // not buffered or rewritten.
    let streaming = call(
        &router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({ "logicalId": "agent-sse", "stream": true })),
        Some(&format!("Bearer {TOKEN}")),
        &[("openai-beta", "agents=v1")],
    )
    .await;
    assert_eq!(streaming.status(), StatusCode::OK);
    assert!(streaming
        .headers()
        .get("content-type")
        .expect("content-type")
        .to_str()
        .expect("ascii")
        .starts_with("text/event-stream"));
    let first_frame = read_until_sse_frame(streaming.into_body()).await;
    assert!(
        first_frame.contains("event: session"),
        "the SSE frame must arrive through the original body: {first_frame}"
    );

    // Rejected request with canary headers; the query canary on an unmatched
    // route must not be logged either.
    assert_eq!(
        call(
            &router,
            Method::GET,
            "/v1/agents/sessions",
            None,
            Some(&format!("Bearer {AUTH_CANARY}")),
            &[("cookie", &format!("session={COOKIE_CANARY}"))],
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &router,
            Method::GET,
            &format!("/not-a-route?q={QUERY_CANARY}"),
            None,
            Some(&format!("Bearer {TOKEN}")),
            &[],
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );

    // Flush every provider and stop; no sleeps are needed because force_flush
    // waits for the export result.
    guard.force_flush();
    let requests = stub.requests.lock().expect("stub lock").clone();
    let payloads = payloads(&requests);

    let paths: Vec<&str> = requests
        .iter()
        .map(|request| request.path.as_str())
        .collect();
    assert!(
        paths.iter().any(|path| path.ends_with("/v1/traces")),
        "no trace export reached the stub: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path.ends_with("/v1/metrics")),
        "no metric export reached the stub: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path.ends_with("/v1/logs")),
        "no log export reached the stub: {paths:?}"
    );

    for request in &requests {
        let mut saw_configured_header = false;
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case("x-otlp-test") {
                assert_eq!(value, "present");
                saw_configured_header = true;
            }
            assert_ne!(
                name.to_ascii_lowercase(),
                "authorization",
                "an OTLP request carried an Authorization header"
            );
            assert_ne!(
                name.to_ascii_lowercase(),
                "cookie",
                "an OTLP request carried a Cookie header"
            );
        }
        assert!(
            saw_configured_header,
            "the configured OTLP header was not applied"
        );
    }

    let all_payloads = payloads
        .iter()
        .map(|(_, body)| body.as_str())
        .collect::<Vec<_>>();
    let all_text = all_payloads.join("\n");
    for canary in [AUTH_CANARY, COOKIE_CANARY, BODY_CANARY, QUERY_CANARY] {
        assert!(
            !all_text.contains(canary),
            "telemetry payload leaked {canary}"
        );
    }
    assert!(
        !all_text.contains("/healthz"),
        "the health probe must not be exported"
    );

    // A 50ms batch delay can split one logical signal across several OTLP
    // requests (a slower runner exports earlier), so join every payload for a
    // signal instead of inspecting only the first one.
    let signal_payloads = |suffix: &str| {
        payloads
            .iter()
            .filter(|(path, _)| path.ends_with(suffix))
            .map(|(_, body)| body.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let metrics = signal_payloads("/v1/metrics");
    assert!(
        metrics.contains("peri_swarm.http.server.requests"),
        "requests metric export missing"
    );
    assert!(metrics.contains("peri_swarm.http.server.duration"));
    assert!(metrics.contains("route"));
    assert!(metrics.contains("status"));

    let logs = signal_payloads("/v1/logs");
    assert!(!logs.is_empty(), "log export missing");
    assert!(logs.contains("peri_swarm.http.access"));
    assert!(logs.contains("request_id"));
    assert!(logs.contains("session_id"));
    assert!(logs.contains("agent-meta"));
    assert!(logs.contains("duration_ms"));
    assert!(logs.contains("unmatched"));

    let traces = signal_payloads("/v1/traces");
    assert!(!traces.is_empty(), "trace export missing");
    assert!(traces.contains("/v1/agents/sessions/{logical_id}"));

    drop(guard);
    server.abort();

    for (name, value) in saved_env {
        std::env::set_var(name, value);
    }
}
