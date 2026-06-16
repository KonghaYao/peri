//! Web PTY 终端服务库入口。

use anyhow::Context;
use axum::Router;
use config::Config;
use session_state::SessionState;

pub mod config;
pub mod http_routes;
pub mod pty_session;
pub mod session_state;
pub mod ws_handler;

#[cfg(test)]
mod config_test;
#[cfg(test)]
mod http_routes_test;
#[cfg(test)]
mod pty_session_test;
#[cfg(test)]
mod ws_handler_test;

/// 启动 Web PTY 终端服务。
pub async fn start_server(config: Config) -> anyhow::Result<()> {
    let cwd = config.cwd.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    let state = SessionState::new(Some(cwd), config.initial_cmd.clone());

    let app = Router::new()
        .route("/", axum::routing::get(http_routes::index))
        .route("/ws", axum::routing::get(ws_handler::ws_handler))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .context("failed to bind TCP listener")?;
    let actual_port = listener.local_addr()?.port();
    let url = format!("http://localhost:{}", actual_port);

    tracing::info!("Web PTY server: {}", url);

    // 尝试自动打开浏览器
    open_browser(&url);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

/// 尝试用系统默认浏览器打开 URL。失败时静默跳过。
fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "linux") {
        std::process::Command::new("xdg-open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()
    } else {
        return;
    };

    match result {
        Ok(_) => tracing::info!("browser opened: {}", url),
        Err(e) => tracing::warn!("failed to open browser: {e}"),
    }
}

/// 优雅关闭信号监听。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
