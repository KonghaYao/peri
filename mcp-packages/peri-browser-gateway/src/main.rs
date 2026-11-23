//! 网关进程入口。只启动一个 loopback listener。

use std::process::ExitCode;

#[cfg(unix)]
use peri_browser_gateway::config::Config;
#[cfg(unix)]
use tokio::net::TcpListener;

#[cfg(unix)]
const USAGE: &str = "\
peri-browser-gateway [OPTIONS]

  --bind IP:PORT                 127.0.0.1:7391（PERI_BROWSER_MONITOR_BIND）
  --cdp-url URL                  Kitesurf 建议地址（KITESURF_CDP_URL）；不自动连接
  --secret VALUE                 monitor token 派生（优先用 PERI_BROWSER_GATEWAY_SECRET）
  --idle-yield-ms MS             默认 15000（PERI_BROWSER_IDLE_YIELD_MS）
  --yield-timeout-ms MS          默认 60000（PERI_BROWSER_YIELD_TIMEOUT_MS）
  --request-human-timeout-ms MS  默认 300000（PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS）
  -h, --help                     显示帮助";

#[cfg(unix)]
#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.as_slice(), [flag] if flag == "-h" || flag == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = async {
        let config = Config::parse_from(args, |key| std::env::var(key).ok())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let listener = TcpListener::bind(config.bind).await?;
        tracing::info!(bind = %listener.local_addr()?, "peri-browser-gateway listening");
        peri_browser_gateway::serve(listener, config, async move {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        })
        .await
    }
    .await;
    if let Err(error) = result {
        eprintln!("peri-browser-gateway: {error:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("peri-browser-gateway: Unix platform required");
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
    let gateway_only = filter_fn(|meta| meta.target().starts_with("peri_browser_gateway"));
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_filter(filter)
                .with_filter(gateway_only),
        )
        .try_init();
}
