use std::future::IntoFuture;

use axum::{routing::get, Json, Router};
use tokio::net::TcpListener;

use super::*;
use crate::config::Config;

fn disconnected() -> (Arc<BrowserCore>, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let config = Config::parse_from(
        ["--cdp-url".to_owned(), "http://127.0.0.1:1".to_owned()],
        |key| match key {
            "HOME" => Some(home.path().to_string_lossy().into_owned()),
            "PERI_BROWSER_CONNECT_TIMEOUT_MS" => Some("100".to_owned()),
            _ => None,
        },
    )
    .unwrap();
    (
        BrowserCore::new(
            Arc::new(config.browser),
            "http://127.0.0.1:2/".to_owned(),
            CancellationToken::new(),
        ),
        home,
    )
}

#[tokio::test]
async fn fresh_sessions_have_independent_identity_and_empty_read_models() {
    let (a, _home_a) = disconnected();
    let (b, _home_b) = disconnected();
    assert_ne!(a.session_id(), b.session_id());
    assert_eq!(
        a.list_pages().await,
        json!({"count": 0, "activePageId": null, "pages": []})
    );
    assert_eq!(a.list_pages().await, b.list_pages().await);
    assert!(matches!(
        a.tool_page(None).await,
        Err(BrowserError::NotConnected)
    ));
    assert!(
        matches!(a.tool_page(Some("foreign")).await, Err(BrowserError::PageNotFound(id)) if id == "foreign")
    );
    assert!(matches!(
        a.yield_page().await,
        Err(BrowserError::NotConnected)
    ));
    assert!(matches!(
        a.request_human("help", None).await,
        Err(BrowserError::NotConnected)
    ));
    assert!(a.service_tasks.lock().unwrap().is_none());
}

#[tokio::test]
async fn bind_race_commits_one_endpoint_without_connecting_a_peer_session() -> anyhow::Result<()> {
    let listener_a = TcpListener::bind("127.0.0.1:0").await?;
    let listener_b = TcpListener::bind("127.0.0.1:0").await?;
    let url_a = format!("http://{}", listener_a.local_addr()?);
    let url_b = format!("http://{}", listener_b.local_addr()?);
    let app = Router::new().route(
        "/json/version",
        get(|| async { Json(json!({"Browser": "Fake/1.0"})) }),
    );
    let stop = CancellationToken::new();
    let server_a = tokio::spawn(
        axum::serve(listener_a, app.clone())
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    );
    let server_b = tokio::spawn(
        axum::serve(listener_b, app)
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    );
    let (core, _home) = disconnected();
    let (peer, _peer_home) = disconnected();
    let (a, b) = tokio::join!(core.connect(&url_a), core.connect(&url_b));
    let (winner, loser, origin) = if a.is_ok() {
        (a?, b, url_a)
    } else {
        (b?, a, url_b)
    };
    let endpoint = onboarding::normalize_endpoint(&origin)?;
    assert_eq!(winner["endpoint"], endpoint);
    assert_eq!(winner["engine"]["version"]["Browser"], "Fake/1.0");
    assert!(winner["engine"]["pageCount"].is_null());
    assert!(
        matches!(loser, Err(BrowserError::AlreadyConnected { endpoint: bound, .. }) if bound == endpoint)
    );
    assert_eq!(
        core.connect(&format!("{origin}/ignored?old=1")).await?["alreadyConnected"],
        true
    );
    assert!(matches!(peer.endpoint(), Err(BrowserError::NotConnected)));
    assert!(matches!(
        core.yield_page().await,
        Err(BrowserError::NoActivePage)
    ));
    assert!(matches!(
        core.request_human("help", None).await,
        Err(BrowserError::NoActivePage)
    ));
    assert!(
        core.service_tasks.lock().unwrap().is_none(),
        "connect、查询和无页交接不启动 CDP worker"
    );
    stop.cancel();
    server_a.await??;
    server_b.await??;
    let status = core.status().await;
    assert_eq!(status["connected"], false);
    assert_eq!(status["endpoint"], endpoint);
    assert_eq!(status["pageCount"], 0);
    Ok(())
}
