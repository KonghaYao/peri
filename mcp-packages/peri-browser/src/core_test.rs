use std::path::PathBuf;
use std::time::Duration;

use super::*;
use crate::config::BrowserConfig;

fn config(cdp_url: Option<&str>, gateway_sock: Option<PathBuf>, lock: PathBuf) -> BrowserConfig {
    BrowserConfig {
        cdp_url: cdp_url.map(str::to_owned),
        owner: "test-owner".to_owned(),
        session_name: "test-session".to_owned(),
        gateway_sock,
        provision_lock: lock,
        provision_lock_wait: Duration::from_millis(200),
        lock_stale_after: Duration::from_secs(1),
        connect_timeout: Duration::from_millis(200),
        page_wait: Duration::from_millis(500),
        idle_yield: Duration::from_millis(50),
        reclaim_wait: Duration::from_millis(100),
        yield_timeout: Duration::from_millis(150),
        request_human_timeout: Duration::from_millis(200),
        http: None,
    }
}

fn offline_core() -> (Arc<BrowserCore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(None, None, dir.path().join("provision.lock")));
    (core, dir)
}

#[tokio::test]
async fn status_reports_a_disconnected_instance() {
    let (core, _dir) = offline_core();
    let status = core.status().await;
    assert_eq!(status["connected"], false);
    assert_eq!(status["pageCount"], 0);
    assert_eq!(status["state"], "idle");
    assert_eq!(status["monitor"], "unavailable");
    assert_eq!(status["yielded"], false);
    assert!(status["endpoint"].is_null());
    assert!(status["engine"].is_null());
    assert!(status["activePageId"].is_null());
    assert_eq!(status["pages"].as_array().map(Vec::len), Some(0));
    assert_eq!(status["sessionId"], core.session_id());
}

#[tokio::test]
async fn list_pages_starts_empty() {
    let (core, _dir) = offline_core();
    let pages = core.list_pages().await;
    assert_eq!(pages["count"], 0);
    assert!(pages["activePageId"].is_null());
    assert!(pages["pages"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn browser_tools_need_an_endpoint() {
    let (core, _dir) = offline_core();

    let error = core.tool_page(None).await.expect_err("no endpoint");
    assert_eq!(error.code(), "not_connected");

    let error = core
        .new_page(Some("https://example.test"))
        .await
        .expect_err("no endpoint");
    assert_eq!(error.code(), "not_connected");

    let error = core.close_page(None).await.expect_err("no pages");
    assert_eq!(error.code(), "no_active_page");

    let error = core.select_page("missing").await.expect_err("no pages");
    assert_eq!(error.code(), "page_not_found");

    // Page ids are matched by prefix only when the reference is unique.
    let error = core
        .select_page("missing-target")
        .await
        .expect_err("unknown page");
    assert_eq!(error.code(), "page_not_found");
}

#[tokio::test]
async fn new_page_rejects_a_non_websocket_endpoint() {
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(
        Some("http://127.0.0.1:1"),
        None,
        dir.path().join("provision.lock"),
    ));

    let error = core.new_page(None).await.expect_err("http is not CDP");
    assert_eq!(error.code(), "invalid_argument");
    assert!(error.to_string().contains("ws:// or wss://"), "{error}");
    assert!(!dir.path().join("provision.lock").exists(), "lock released");
}

#[tokio::test]
async fn new_page_reports_an_unreachable_websocket_endpoint() {
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(
        Some("ws://127.0.0.1:1"),
        None,
        dir.path().join("provision.lock"),
    ));

    let error = core.new_page(None).await.expect_err("nothing listens");
    assert!(
        matches!(error.code(), "browser_control" | "timeout"),
        "unexpected code {}: {error}",
        error.code()
    );
}

#[tokio::test]
async fn status_reports_an_unreachable_engine() {
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(
        Some("ws://127.0.0.1:1"),
        None,
        dir.path().join("provision.lock"),
    ));

    let status = core.status().await;
    assert_eq!(status["connected"], false);
    assert_eq!(status["endpoint"], "ws://127.0.0.1:1");
    assert_eq!(status["engine"]["reachable"], false);
    assert!(status["engine"]["error"].is_string());
}

#[tokio::test]
async fn yield_page_needs_a_monitor_gateway() {
    let (core, _dir) = offline_core();
    let error = core.yield_page().await.expect_err("no gateway");
    assert_eq!(error.code(), "gateway_unavailable");
}

#[tokio::test]
async fn yield_page_needs_a_page_once_the_gateway_is_configured() {
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(
        Some("ws://127.0.0.1:1"),
        Some(dir.path().join("gateway.sock")),
        dir.path().join("provision.lock"),
    ));

    let error = core.yield_page().await.expect_err("no page yet");
    assert_eq!(error.code(), "no_active_page");
}

#[tokio::test]
async fn request_human_needs_a_connected_gateway() {
    let (core, _dir) = offline_core();
    let error = core
        .request_human("log in", None)
        .await
        .expect_err("no gateway");
    assert_eq!(error.code(), "gateway_unavailable");

    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(
        Some("ws://127.0.0.1:1"),
        Some(dir.path().join("missing-gateway.sock")),
        dir.path().join("provision.lock"),
    ));
    let error = core
        .request_human("log in", Some(100))
        .await
        .expect_err("gateway is not listening");
    assert_eq!(error.code(), "gateway_unavailable");
}

#[tokio::test]
async fn idle_yield_arming_is_a_no_op_without_a_gateway() {
    let (core, _dir) = offline_core();
    assert_eq!(core.agent_state().await, AgentState::Idle);
    core.touch();
    core.touch();
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(core.agent_state().await, AgentState::Idle);
}

#[tokio::test]
async fn syncing_pages_without_a_gateway_is_a_no_op() {
    let (core, _dir) = offline_core();
    core.sync_pages().await;
    assert_eq!(core.agent_state().await, AgentState::Idle);
}

#[tokio::test]
async fn sessions_get_their_own_ids() {
    let (first, _dir) = offline_core();
    let (second, _dir) = offline_core();
    assert_ne!(first.session_id(), second.session_id());
    assert_eq!(first.session_id().len(), 36);
}
