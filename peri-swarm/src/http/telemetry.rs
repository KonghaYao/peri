//! HTTP observability middleware (stage 3).
//!
//! One outermost layer records a server span, a request counter, a latency
//! histogram and one access-log line. It deliberately never touches the request
//! or response body: `next.run(request)` returns as soon as the response headers
//! exist, so SSE responses keep streaming through the original body type and no
//! global timeout is introduced.
//!
//! `/healthz` is skipped entirely (a supervisor probe must stay free of
//! telemetry side effects). `/events` skips the latency histogram and emits at
//! most one "stream opened" access line: a streaming request's useful duration
//! is the lifetime of the body, which this middleware cannot observe without
//! wrapping it.
//!
//! The access log never records headers, query strings, bodies, `Authorization`,
//! `Cookie` or idempotency keys. The gateway has no inbound request-id contract,
//! so a per-request id is generated instead of reading one from a header; the
//! session id is parsed from the matched path only when it is a plain
//! identifier.

use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

use super::AppState;
use crate::telemetry::AccessLog;

pub async fn observe(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(telemetry) = state.telemetry().cloned() else {
        return next.run(request).await;
    };

    let path = request.uri().path().to_owned();
    if path == "/healthz" {
        return next.run(request).await;
    }

    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |path| path.as_str().to_owned());
    let session_id = if route.starts_with("/v1/agents/sessions/") {
        session_id_from_path(&path)
    } else {
        None
    };
    let request_id = Uuid::now_v7().to_string();
    let span = telemetry.start_span(&method, &route, session_id);
    let started = Instant::now();

    // The response body is handed back untouched; this is where observability
    // must stop if it wants SSE to keep working.
    let response = next.run(request).await;
    let elapsed = started.elapsed();
    let status = response.status().as_u16();
    let record_duration = !is_event_stream(&route);

    telemetry.record_request(&method, &route, status, elapsed, record_duration);
    let trace_context = span.as_ref().map(|span| span.context().clone());
    if let Some(span) = span {
        span.end(status);
    }
    telemetry.log_access(
        &AccessLog {
            method: &method,
            route: &route,
            status,
            duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            session_id,
            request_id: &request_id,
        },
        trace_context.as_ref(),
    );
    response
}

fn is_event_stream(route: &str) -> bool {
    route.ends_with("/events")
}

/// `/v1/agents/sessions/{logical_id}` → `Some(logical_id)` when every byte is a
/// plain identifier character. Encoded or unexpected input is dropped rather
/// than decoded or logged.
fn session_id_from_path(path: &str) -> Option<&str> {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    while let Some(segment) = segments.next() {
        if segment == "sessions" {
            return segments.next().filter(|id| is_plain_identifier(id));
        }
    }
    None
}

fn is_plain_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
#[path = "telemetry_test.rs"]
mod tests;
