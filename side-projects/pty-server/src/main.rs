use pty_server::config::Config;
use pty_server::session_state::SessionState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().try_init().ok();
    let cfg = Config::from_args();

    // 未指定 --cwd 时使用启动 pty-server 时的当前目录
    let cwd = cfg.cwd.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    let state = SessionState::new(Some(cwd), cfg.initial_cmd.clone());

    let app = axum::Router::new()
        .route("/", axum::routing::get(pty_server::http_routes::index))
        .route(
            "/ws",
            axum::routing::get(pty_server::ws_handler::ws_handler),
        )
        .with_state(state);

    let addr = format!("0.0.0.0:{}", cfg.port);
    tracing::info!(
        "PTY server listening on http://{} cwd={:?}",
        addr,
        cfg.cwd.as_deref().unwrap_or("current")
    );

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

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
