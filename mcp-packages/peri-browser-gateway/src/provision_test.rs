use std::time::Duration;

use axum::routing::get;
use axum::Json;
use serde_json::json;

use super::*;

fn settings(cdp_url: &str, lock: std::path::PathBuf) -> ProvisionSettings {
    ProvisionSettings {
        cdp_url: cdp_url.to_owned(),
        owner: "peri-browser-test".to_owned(),
        lock,
        lock_wait: Duration::from_millis(50),
        stale_after: Duration::from_secs(30),
        connect_timeout: Duration::from_secs(1),
    }
}

#[test]
fn the_browser_endpoint_preserves_the_instance_query() {
    assert_eq!(
        browser_ws_url("ws://127.0.0.1:8827", "peri-browser").unwrap(),
        "ws://127.0.0.1:8827/devtools/browser?owner=peri-browser"
    );
    assert_eq!(
        browser_ws_url("ws://127.0.0.1:8827/devtools/browser?session=s1", "peri").unwrap(),
        "ws://127.0.0.1:8827/devtools/browser?session=s1&owner=peri"
    );
    let error = browser_ws_url("http://127.0.0.1:8827", "peri").unwrap_err();
    assert!(error.to_string().contains("expected ws:// or wss://"));
}

#[tokio::test]
async fn the_create_lock_is_exclusive_and_removed_on_drop() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("provision.lock");
    let first = ProvisionLock::acquire(&path, Duration::from_millis(10), Duration::from_secs(30))
        .await
        .expect("first lock");
    assert!(path.exists(), "the lock file records its owner");

    let error = ProvisionLock::acquire(&path, Duration::from_millis(20), Duration::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(error, BrowserError::LockTimeout(_)));

    drop(first);
    assert!(!path.exists(), "drop removes the lock");
    let _again = ProvisionLock::acquire(&path, Duration::from_millis(10), Duration::from_secs(30))
        .await
        .expect("the lock is reusable after drop");
}

#[tokio::test]
async fn a_stale_lock_is_reclaimed() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("stale.lock");
    std::fs::write(&path, "pid=1").expect("seed a stale lock");
    // Any age is stale when the threshold is zero.
    let lock = ProvisionLock::acquire(&path, Duration::from_millis(100), Duration::ZERO)
        .await
        .expect("stale lock is removed");
    drop(lock);
    assert!(!path.exists());
}

#[tokio::test]
async fn an_unwritable_lock_directory_is_an_internal_error() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("missing").join("provision.lock");
    let error = ProvisionLock::acquire(&path, Duration::from_millis(10), Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(matches!(error, BrowserError::Internal(_)));
}

#[tokio::test]
async fn the_engine_probe_reads_only_version_and_never_enumerates_pages() {
    async fn forbidden_list() -> Json<serde_json::Value> {
        panic!("引擎全量页面列表不属于探测契约");
    }
    let app = axum::Router::new()
        .route(
            "/json/version",
            get(|| async { Json(json!({"Browser": "Fake/1.0", "webSocketDebuggerUrl": "ws://other.invalid:9000"})) }),
        )
        .route("/json/list", get(forbidden_list));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let probe = probe_engine(
        &format!("ws://127.0.0.1:{}", address.port()),
        Duration::from_secs(2),
    )
    .await;
    assert!(probe.reachable, "{probe:?}");
    assert_eq!(probe.page_count, None);
    let version = probe.version.unwrap();
    assert_eq!(version, json!({"Browser": "Fake/1.0"}));
    assert!(probe.error.is_none());
    server.abort();
}

#[tokio::test]
async fn an_unreachable_engine_probe_reports_the_reason() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let probe = probe_engine(
        &format!("ws://127.0.0.1:{port}"),
        Duration::from_millis(200),
    )
    .await;
    assert!(!probe.reachable);
    assert!(probe.error.is_some());
    assert_eq!(probe.page_count, None);
}

#[tokio::test]
async fn an_http_endpoint_is_never_probed_as_a_browser() {
    let probe = probe_engine("http://127.0.0.1:9", Duration::from_millis(50)).await;
    assert!(!probe.reachable);
    assert!(probe
        .error
        .unwrap_or_default()
        .contains("unsupported CDP scheme"));
}

#[tokio::test]
async fn provision_settings_build_the_browser_url_from_the_endpoint() {
    let directory = tempfile::tempdir().expect("temp dir");
    let settings = settings("ws://127.0.0.1:8827", directory.path().join("lock"));
    assert_eq!(
        settings.browser_url().unwrap(),
        "ws://127.0.0.1:8827/devtools/browser?owner=peri-browser-test"
    );
}
