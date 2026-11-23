//! `peri-browser` process entry.
//!
//! Default transport is MCP stdio. `--http <addr>` serves the same tools,
//! skills and resources over MCP Streamable HTTP at `/mcp`. Logs always go to
//! stderr: stdout is the stdio transport.

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use clap::Parser;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    use rmcp::ServiceExt;

    use peri_browser::config::{BrowserConfig, Cli};
    use peri_browser::core::BrowserCore;
    use peri_browser::server::BrowserMcpServer;

    init_tracing();
    let cli = Cli::parse();
    let config = BrowserConfig::from_env(cli.http).context("invalid configuration")?;
    let http = config.http;
    let core = BrowserCore::new(config);
    tracing::info!(
        session = %core.session_id(),
        transport = if http.is_some() { "http" } else { "stdio" },
        "peri-browser starting; call browser_status and browser_connect to attach a browser"
    );

    if let Some(address) = http {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .with_context(|| format!("cannot bind MCP HTTP listener at {address}"))?;
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut server_config = StreamableHttpServerConfig::default();
        server_config.legacy_session_mode = true;
        server_config.cancellation_token = shutdown.clone();
        let factory_core = std::sync::Arc::clone(&core);
        let service: StreamableHttpService<BrowserMcpServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(BrowserMcpServer::new(std::sync::Arc::clone(&factory_core))),
                Default::default(),
                server_config,
            );
        let router = axum::Router::new().nest_service("/mcp", service);
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                shutdown_signal().await;
                shutdown.cancel();
            })
            .await
            .context("MCP HTTP server failed")?;
    } else {
        let service = BrowserMcpServer::new(std::sync::Arc::clone(&core))
            .serve((tokio::io::stdin(), tokio::io::stdout()))
            .await
            .context("MCP stdio initialization failed")?;
        service
            .waiting()
            .await
            .context("MCP stdio serving failed")?;
    }

    core.gateway.unregister();
    tracing::info!("peri-browser stopped");
    Ok(())
}

#[cfg(unix)]
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("peri_browser=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(unix)]
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(error = %error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("peri-browser requires a Unix system (the monitor registry uses a Unix socket)");
    std::process::exit(1);
}
