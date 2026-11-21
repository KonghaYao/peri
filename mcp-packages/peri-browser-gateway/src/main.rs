//! `peri-browser-gateway` process entry.
//!
//! Starts the UDS registry and the monitor HTTP server, and shuts both down on
//! SIGINT/SIGTERM. Configuration errors fail startup instead of silently
//! falling back to a different port or socket.

use std::process::ExitCode;

#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use peri_browser_gateway::config::{prepare_registry_socket, Config};
#[cfg(unix)]
use peri_browser_gateway::dev;
#[cfg(unix)]
use peri_browser_gateway::http::{self, AppState};
#[cfg(unix)]
use peri_browser_gateway::ownership::{OwnershipStore, Settings};
#[cfg(unix)]
use peri_browser_gateway::registry::{self, RegistryContext, RegistryHandle};
#[cfg(unix)]
use peri_browser_gateway::session::SessionStore;
#[cfg(unix)]
use tokio::net::{TcpListener, UnixListener};
#[cfg(unix)]
use tracing::{info, warn};

#[cfg(unix)]
const USAGE: &str = "\
peri-browser-gateway - monitor gateway for the peri-browser MCP

USAGE:
    peri-browser-gateway --cdp-url <ws://host:port> [OPTIONS]

OPTIONS:
    --cdp-url <url>          Kitesurf CDP endpoint (or KITESURF_CDP_URL)
    --bind <addr>            monitor listen address (or PERI_BROWSER_MONITOR_BIND)
                             default 127.0.0.1:7391
    --registry-sock <path>   MCP <-> gateway Unix socket (or PERI_BROWSER_GATEWAY_SOCK)
                             default <temp dir>/peri-browser-gateway.sock
    --secret <value>         token derivation secret (or PERI_BROWSER_GATEWAY_SECRET)
                             when absent, a random secret is persisted next to
                             the registry socket so monitor URLs survive restarts
    --dev                    standalone: discover pages from <cdp-url>/json/list
                             instead of waiting for an MCP registration
    --idle-yield-ms <ms>     ask the agent for the controller after this much
                             idle time (or PERI_BROWSER_IDLE_YIELD_MS)
                             default 15000
    --yield-timeout-ms <ms>  wait for the agent to finish its call before
                             reporting yield_pending (or
                             PERI_BROWSER_YIELD_TIMEOUT_MS), default 5000
    --request-human-timeout-ms <ms>
                             wait for a human answer before returning timeout
                             (or PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS)
                             default 120000
    -h, --help               print this help";

#[cfg(unix)]
#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let config = match Config::parse_from(args, |key| std::env::var(key).ok()) {
        Ok(config) => config,
        Err(error) => {
            // stderr stays visible even with logging turned down.
            eprintln!("peri-browser-gateway: {error}");
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("peri-browser-gateway: {error:#}");
            tracing::error!(error = %error, "peri-browser-gateway failed");
            ExitCode::FAILURE
        }
    }
}

/// The registry transport is a Unix-domain socket; other targets have no
/// equivalent capability, so the process fails instead of silently running
/// without the MCP control channel.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("peri-browser-gateway: Unix-domain sockets are required (Unix only)");
    ExitCode::FAILURE
}

#[cfg(unix)]
async fn run(config: Config) -> anyhow::Result<()> {
    // Resolve the secret before creating the socket: a bad secret file must
    // fail startup without leaving a registry socket behind.
    let secret: Arc<[u8]> = Arc::from(config.resolve_secret()?.into_bytes());

    prepare_registry_socket(&config.registry_sock)?;
    let registry_listener = UnixListener::bind(&config.registry_sock)?;
    restrict_registry_socket(&config.registry_sock)?;

    let handle = Arc::new(RegistryHandle::new());
    let settings = Settings::from_config(&config);
    let ownership = Arc::new(OwnershipStore::new(settings, handle.clone()));
    let sessions = Arc::new(SessionStore::new(secret, ownership));
    let monitor_base = config.monitor_base_url();

    tokio::spawn(registry::serve(
        registry_listener,
        RegistryContext {
            sessions: sessions.clone(),
            handle: handle.clone(),
            monitor_base: monitor_base.clone(),
        },
    ));

    if config.dev {
        dev::start(
            sessions.clone(),
            config.cdp_url.clone(),
            config.dev_http_base(),
            monitor_base.clone(),
        )
        .await;
    }

    let app = http::build_router(AppState {
        sessions,
        registry: handle,
    });
    let http_listener = TcpListener::bind(config.bind).await?;

    info!(
        bind = %config.bind,
        registry_sock = %config.registry_sock.display(),
        cdp_url = %config.cdp_url,
        idle_yield_ms = config.idle_yield_ms,
        yield_timeout_ms = config.yield_timeout_ms,
        request_human_timeout_ms = config.request_human_timeout_ms,
        "peri-browser-gateway listening"
    );
    info!(url = %format!("{monitor_base}/"), "session list");
    if !config.bind.ip().is_loopback() {
        warn!(
            bind = %config.bind,
            "listening on a non-loopback address: the monitor exposes browser content"
        );
    }

    axum::serve(http_listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    let _ = std::fs::remove_file(&config.registry_sock);
    info!("peri-browser-gateway stopped");
    Ok(())
}

#[cfg(unix)]
fn restrict_registry_socket(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(unix)]
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("peri_browser_gateway=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[cfg(unix)]
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(error = %error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
