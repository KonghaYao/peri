//! Generic in-process serving for builtin MCP instances.
//!
//! `peri mcp-start` uses these helpers to serve any `rmcp::ServerHandler` without a
//! sibling worker binary. HTTP is MCP Streamable HTTP mounted at `/mcp`.

use std::io;

use anyhow::{Context, Result};
use rmcp::{
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ServerHandler, ServiceExt,
};
use tokio_util::sync::CancellationToken;

/// Serve `server` over stdin/stdout until the peer closes the transport.
pub async fn serve_stdio<S>(server: S) -> Result<()>
where
    S: ServerHandler + Send + 'static,
{
    let service = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .context("MCP stdio initialization failed")?;
    service
        .waiting()
        .await
        .context("MCP stdio serving failed")
        .map(|_| ())
}

/// Serve a fresh `server` per session over MCP Streamable HTTP at `/mcp`.
///
/// `allowed_hosts` extends rmcp's loopback-only `Host` allowlist. Passing `"*"`
/// disables the check; leave empty to keep the loopback default. Shutdown is
/// graceful on Ctrl-C / SIGTERM.
pub async fn serve_http<S, F>(
    factory: F,
    host: &str,
    port: u16,
    allowed_hosts: &[String],
) -> Result<()>
where
    S: ServerHandler + Send + 'static,
    F: Fn() -> Result<S, io::Error> + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("cannot bind MCP HTTP listener at {host}:{port}"))?;
    let shutdown = CancellationToken::new();
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = true;
    config.cancellation_token = shutdown.clone();
    if allowed_hosts.iter().any(|value| value == "*") {
        config = config.disable_allowed_hosts();
    } else if !allowed_hosts.is_empty() {
        let hosts = ["localhost", "127.0.0.1", "::1"]
            .into_iter()
            .map(str::to_owned)
            .chain(allowed_hosts.iter().cloned());
        config = config.with_allowed_hosts(hosts);
    }
    let service: StreamableHttpService<S, LocalSessionManager> =
        StreamableHttpService::new(factory, Default::default(), config);
    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown.cancel();
        })
        .await
        .context("MCP HTTP server failed")
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
