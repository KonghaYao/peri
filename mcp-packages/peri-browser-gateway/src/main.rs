//! `peri-browser` entry point: serve the gateway in the foreground, run it in
//! the background, stop it, install/start/update kitesurf, or re-run the
//! browser installer.

use std::process::ExitCode;

#[cfg(unix)]
use peri_browser_gateway::cli::{self, Command, StartOptions};
#[cfg(unix)]
use peri_browser_gateway::config::Config;
#[cfg(unix)]
use peri_browser_gateway::daemon::{self, EngineBinding};
#[cfg(unix)]
use peri_browser_gateway::lifecycle;
#[cfg(unix)]
use tokio::net::TcpListener;

#[cfg(unix)]
#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("peri-browser: {error}");
            return ExitCode::FAILURE;
        }
    };
    match run(command).await {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("peri-browser: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
async fn run(command: Command) -> anyhow::Result<i32> {
    match command {
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(0)
        }
        Command::Version => {
            println!("peri-browser {}", peri_browser_gateway::version());
            Ok(0)
        }
        Command::Serve(args) => {
            serve(args).await?;
            Ok(0)
        }
        Command::StartGateway(args) => {
            daemon::start_gateway(args, &process_env, None).await?;
            Ok(0)
        }
        Command::Stop(options) => daemon::stop(options.bind, &process_env).await,
        Command::InstallKitesurf => lifecycle::install(&process_env),
        Command::Update => update(),
        Command::UpdateKitesurf => lifecycle::update_kitesurf(&process_env),
        Command::StartKitesurf(options) => {
            let owned = lifecycle::ensure_engine(options.port, &process_env).await?;
            let engine = EngineBinding {
                port: options.port,
                owned,
            };
            daemon::start_gateway(gateway_args(&options), &process_env, Some(engine)).await?;
            Ok(0)
        }
    }
}

/// The documented install one-liner. `pipefail` is required: without it a
/// failed `curl` feeds an empty script to `bash`, which exits 0.
#[cfg(unix)]
const UPDATE_PIPELINE: &str =
    "set -o pipefail; curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install-browser.sh | bash";

/// `update`: re-run the official installer with inherited stdio and environment
/// (`PERI_BROWSER_INSTALL_DIR`, `PERI_BROWSER_SKIP_KITESURF`, ...), and return
/// the script's exit code unchanged. The running gateway keeps its old binary.
#[cfg(unix)]
fn update() -> anyhow::Result<i32> {
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(UPDATE_PIPELINE)
        .status()
        .map_err(|error| {
            anyhow::anyhow!("failed to run `bash`: {error}; `update` needs bash and curl on PATH")
        })?;
    let code = status.code().unwrap_or(1);
    if code != 0 {
        eprintln!(
            "peri-browser: update failed (exit code {code}); check network access and that `curl` and `bash` are on PATH"
        );
    }
    Ok(code)
}

#[cfg(unix)]
async fn serve(args: Vec<String>) -> anyhow::Result<()> {
    let config = Config::parse_from(args, process_env)?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let listener = TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %listener.local_addr()?, "peri-browser listening");
    peri_browser_gateway::serve(listener, config, async move {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    })
    .await
}

/// The background gateway uses the same listener and suggested engine as the
/// explicit `start kitesurf` flags.
#[cfg(unix)]
fn gateway_args(options: &StartOptions) -> Vec<String> {
    vec![
        "--bind".to_owned(),
        options.bind.to_string(),
        "--cdp-url".to_owned(),
        format!("http://127.0.0.1:{}", options.port),
    ]
}

#[cfg(unix)]
fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("peri-browser: Unix platform required");
    ExitCode::FAILURE
}

#[cfg(unix)]
fn init_tracing() {
    use tracing_subscriber::filter::filter_fn;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    // SDK、WebSocket 与 HTTP 依赖的 span 可以含原始 URL、header 和 payload。
    // RUST_LOG 只调整本网关的结构化诊断级别。
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // 二进制 target 是 `peri_browser`，库 target 是 `peri_browser_gateway`。
    let gateway_only = filter_fn(|meta| meta.target().starts_with("peri_browser"));
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_filter(filter)
                .with_filter(gateway_only),
        )
        .try_init();
}
