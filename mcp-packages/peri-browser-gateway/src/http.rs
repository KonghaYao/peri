//! HTTP surface: session list, health probe, monitor page and its WebSocket.
//!
//! The monitor page and its socket are addressed by the per-session token in the
//! path. An unknown token answers `404`, exactly like an unauthorized one: the
//! port must not become a session-existence oracle (README §6.6).
//!
//! Headers on both HTML responses are the live-view baseline: no caching, no
//! referrer (the token is in the URL), no framing, no MIME sniffing.

use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use axum::Router;
use serde::Serialize;

use crate::error::GatewayError;
use crate::registry::RegistryHandle;
use crate::session::{Session, SessionStore};
use crate::ws;

/// Single self-contained monitor page (README §6.8).
pub const MONITOR_HTML: &str = include_str!("../assets/monitor.html");

#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<SessionStore>,
    pub registry: Arc<RegistryHandle>,
}

/// Builds the full router. Kept separate from the process entry so tests can
/// serve it on an ephemeral port.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/s/{token}/", get(monitor))
        .route("/s/{token}/ws", get(ws_upgrade))
        .with_state(state)
}

async fn index(State(state): State<AppState>) -> Response {
    let sessions = state.sessions.list().await;
    html_response(render_index(&sessions))
}

#[derive(Serialize)]
struct Health {
    ok: bool,
    sessions: usize,
}

async fn healthz(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        ok: true,
        sessions: state.sessions.len().await,
    })
}

async fn monitor(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, GatewayError> {
    let _session = state
        .sessions
        .lookup_by_token(&token)
        .await
        .ok_or(GatewayError::UnknownToken)?;
    Ok(html_response(MONITOR_HTML.to_owned()))
}

async fn ws_upgrade(
    State(state): State<AppState>,
    Path(token): Path<String>,
    request: Request,
) -> Result<Response, GatewayError> {
    // Check the token before looking at the upgrade headers: an unknown token
    // must be 404 even for a plain GET.
    let session = state
        .sessions
        .lookup_by_token(&token)
        .await
        .ok_or(GatewayError::UnknownToken)?;

    let (mut parts, _body) = request.into_parts();
    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &state)
        .await
        .map_err(|_rejection| GatewayError::BadRequest("websocket upgrade required".to_owned()))?;

    let state = state.clone();
    Ok(upgrade.on_upgrade(move |socket| ws::handle(socket, session, state)))
}

fn html_response(body: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    (headers, body).into_response()
}

/// Renders the session list. Session names come from the MCP and are escaped.
#[must_use]
pub fn render_index(sessions: &[Session]) -> String {
    let mut items = String::new();
    for session in sessions {
        let token = escape_html(&session.token);
        items.push_str(&format!(
            "<li><a href=\"/s/{token}/\">{name}</a> ({pages} page(s), {state})</li>",
            name = escape_html(&session.name),
            pages = session.pages.len(),
            state = session.agent_state.as_str(),
        ));
    }
    if sessions.is_empty() {
        items.push_str("<li>no sessions</li>");
    }
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head><meta charset=\"utf-8\">\
         <title>peri-browser gateway</title></head>\n<body>\n\
         <h1>peri-browser gateway</h1>\n\
         <h2>Sessions ({count})</h2>\n<ul>\n{items}\n</ul>\n\
         </body>\n</html>\n",
        count = sessions.len(),
    )
}

/// Minimal HTML escaping for names and URLs in the session list.
#[must_use]
pub fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
#[path = "http_test.rs"]
mod tests;
