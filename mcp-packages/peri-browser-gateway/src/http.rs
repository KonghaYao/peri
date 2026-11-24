//! 同一 TCP listener 的 MCP 与 monitor 路由。Monitor 必须通过当前 Live token。

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::uri::Authority;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::config::{is_bind_ip, Config};
use crate::error::GatewayError;
use crate::session::{SessionDirectory, SETTLE_TIMEOUT};
use crate::{mcp_http, ws};

pub const MONITOR_HTML: &str = include_str!("../assets/monitor.html");

/// 使用实际监听地址构造路由；测试同样经真实的 TCP 入口。
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let bound = listener.local_addr()?;
    anyhow::ensure!(
        is_bind_ip(bound.ip()),
        "gateway requires a loopback listener"
    );
    let keep_alive = config.browser.keep_alive();
    let sessions = Arc::new(SessionDirectory::new(
        config.browser,
        format!("http://{bound}/"),
        config.secret,
    ));
    serve_with_sessions(listener, bound, sessions, keep_alive, shutdown).await
}

async fn serve_with_sessions(
    listener: TcpListener,
    bound: SocketAddr,
    sessions: Arc<SessionDirectory>,
    keep_alive: Duration,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let stop = CancellationToken::new();
    let app = build_router(bound, sessions.clone(), stop.clone(), keep_alive);
    let graceful = stop.clone();
    let serving = axum::serve(listener, app).with_graceful_shutdown(graceful.cancelled_owned());
    let mut serving = std::pin::pin!(serving.into_future());
    let result = tokio::select! {
        result = &mut serving => Some(result),
        () = shutdown => None,
    };
    sessions.begin_shutdown();
    stop.cancel();
    let (http, drained) = tokio::time::timeout(SETTLE_TIMEOUT + Duration::from_secs(2), async {
        tokio::join!(
            async {
                match result {
                    Some(result) => result,
                    None => serving.await,
                }
            },
            sessions.wait_shutdown()
        )
    })
    .await
    .map_err(|_| anyhow::anyhow!("gateway shutdown did not settle before its deadline"))?;
    http?;
    drained?;
    Ok(())
}

fn build_router(
    bound: SocketAddr,
    sessions: Arc<SessionDirectory>,
    stop: CancellationToken,
    keep_alive: Duration,
) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/s/{token}/", get(monitor))
        .route("/s/{token}/ws", get(ws_upgrade))
        .route_service(
            "/mcp",
            mcp_http::service(sessions.clone(), stop, keep_alive),
        )
        .layer(middleware::from_fn_with_state(bound, authority_guard))
        .with_state(sessions)
}

async fn authority_guard(
    State(bound): State<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !allowed_authority(&request, bound) {
        return (StatusCode::FORBIDDEN, "local Host and Origin required").into_response();
    }
    next.run(request).await
}

fn allowed_authority(request: &Request, bound: SocketAddr) -> bool {
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let header_host = match hosts.next() {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|v| v.parse::<Authority>().ok())
        {
            Some(host) => Some(host),
            None => return false,
        },
        None => None,
    };
    let uri_host = request.uri().authority();
    if hosts.next().is_some() || matches!((&header_host, uri_host), (Some(a), Some(b)) if a != b) {
        return false;
    }
    let Some(host) = header_host.as_ref().or(uri_host) else {
        return false;
    };
    let ip = host
        .host()
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>();
    if host.as_str().contains('@')
        || host.port_u16().unwrap_or(80) != bound.port()
        || !(host.host().eq_ignore_ascii_case("localhost") || ip == Ok(bound.ip()))
    {
        return false;
    }
    if request.uri().path() == "/mcp" {
        return true; // rmcp 在空 allowlist 下拒绝任何 Origin。
    }
    let mut origins = request.headers().get_all(header::ORIGIN).iter();
    match origins.next() {
        None => true,
        Some(origin) => {
            origins.next().is_none()
                && origin.to_str().is_ok_and(|origin| {
                    let expected = format!("http://{host}");
                    origin == expected
                })
        }
    }
}

async fn index(State(sessions): State<Arc<SessionDirectory>>) -> Response {
    let entries = sessions.list();
    let mut items = String::new();
    for (token, core) in &entries {
        let (count, state) = core.summary();
        let name = escape_html(&core.name());
        items.push_str(&format!(
            "<li><a href=\"/s/{token}/\">{name}</a> ({count} page(s), {})</li>",
            state.as_str()
        ));
    }
    if entries.is_empty() {
        items.push_str("<li>no sessions</li>");
    }
    html_response(format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>peri-browser gateway</title><h1>peri-browser gateway</h1><h2>Sessions ({})</h2><ul>{items}</ul></html>", entries.len()))
}

async fn healthz(State(sessions): State<Arc<SessionDirectory>>) -> Json<serde_json::Value> {
    Json(json!({"ok": true, "sessions": sessions.list().len()}))
}

async fn monitor(
    State(sessions): State<Arc<SessionDirectory>>,
    Path(token): Path<String>,
) -> Result<Response, GatewayError> {
    sessions
        .by_token(&token)
        .ok_or(GatewayError::UnknownToken)?;
    Ok(html_response(MONITOR_HTML.to_owned()))
}

async fn ws_upgrade(
    State(sessions): State<Arc<SessionDirectory>>,
    Path(token): Path<String>,
    request: Request,
) -> Result<Response, GatewayError> {
    let entry = sessions
        .by_token(&token)
        .ok_or(GatewayError::UnknownToken)?;
    let (core, permit) = entry.admit().ok_or(GatewayError::UnknownToken)?;
    let (mut parts, _body) = request.into_parts();
    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &sessions)
        .await
        .map_err(|_| GatewayError::BadRequest("websocket upgrade required".to_owned()))?;
    Ok(upgrade
        .max_message_size(128 * 1024)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            ws::handle(socket, core).await;
        }))
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
