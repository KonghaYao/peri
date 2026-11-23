use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::config::BrowserConfig;

fn config(gateway_sock: Option<PathBuf>, lock: PathBuf) -> BrowserConfig {
    BrowserConfig {
        owner: "test-owner".to_owned(),
        session_name: "test-session".to_owned(),
        kitesurf_home: std::env::temp_dir().join("peri-browser-core-test-kitesurf"),
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

fn canonical_url(http_url: &str) -> String {
    format!(
        "{}/devtools/browser",
        http_url.replacen("http://", "ws://", 1)
    )
}

fn offline_core() -> (Arc<BrowserCore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(None, dir.path().join("provision.lock")));
    (core, dir)
}

/// A loopback HTTP surface that answers the two probe paths. It has no
/// WebSocket endpoint, so page tools fail after `connect` but the connection
/// state itself is proven.
async fn fake_engine() -> (SocketAddr, String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new()
        .route(
            "/json/version",
            axum::routing::get(|| async { axum::Json(json!({"Browser": "Kitesurf/0.5.4"})) }),
        )
        .route(
            "/json/list",
            axum::routing::get(|| async { axum::Json(json!([{"id": "target-1"}])) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake engine");
    let address = listener.local_addr().expect("fake engine address");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (address, format!("http://{address}"), server)
}

async fn connected_core() -> (
    Arc<BrowserCore>,
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<()>,
) {
    let (_, url, server) = fake_engine().await;
    let (core, dir) = offline_core();
    let value = core
        .connect(&url)
        .await
        .expect("the fake engine is reachable");
    assert_eq!(value["connected"], true);
    assert_eq!(
        core.endpoint().await.expect("endpoint"),
        canonical_url(&url)
    );
    (core, dir, url, server)
}

#[tokio::test]
async fn status_reports_a_disconnected_instance_with_next_steps() {
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

    assert_eq!(status["platform"]["os"], std::env::consts::OS);
    assert_eq!(status["platform"]["arch"], std::env::consts::ARCH);
    assert!(status["platform"]["supported"].is_boolean());
    assert!(status["kitesurf"]["installCommand"]
        .as_str()
        .expect("install command")
        .contains("install-kitesurf.sh"));
    assert!(status["kitesurf"]["onPath"].is_boolean());
    assert_eq!(
        status["recommendedEndpoint"]["url"],
        "http://127.0.0.1:18971"
    );
    assert!(
        status["recommendedEndpoint"]["listening"].is_boolean(),
        "the recommended endpoint is probed while disconnected"
    );

    let steps = status["nextSteps"].as_array().expect("next steps");
    // A real instance may already own 18971 on the test machine, so the serve
    // step is tied to the reported listening state instead of the host.
    if status["recommendedEndpoint"]["listening"] == false {
        assert!(
            steps.iter().any(|step| step
                .as_str()
                .is_some_and(|step| step == "kitesurf serve --port 18971")),
            "a disconnected session with no listener is told to start kitesurf: {steps:?}"
        );
    }
    assert!(steps.iter().any(|step| step
        .as_str()
        .is_some_and(|step| step.starts_with("browser_connect(url:"))));
    // Install detection reads the real PATH, so whether the install step is
    // present depends on the machine. When it is present it must come first.
    if status["kitesurf"].get("installInstruction").is_some() {
        assert!(
            steps[0]
                .as_str()
                .is_some_and(|step| step.contains("install-kitesurf.sh")),
            "the install command is the first step: {steps:?}"
        );
    }
}

#[tokio::test]
async fn status_reports_the_detected_install_version() {
    let install = tempfile::tempdir().expect("install root");
    std::fs::create_dir_all(install.path().join("bin")).expect("bin dir");
    std::fs::write(install.path().join("bin/kitesurf"), b"#!/bin/sh\n").expect("binary");
    std::fs::write(install.path().join("current-version.txt"), "v0.5.4\n").expect("version");

    let dir = tempfile::tempdir().expect("temp dir");
    let mut settings = config(None, dir.path().join("provision.lock"));
    settings.kitesurf_home = install.path().to_path_buf();
    let core = BrowserCore::new(settings);

    let status = core.status().await;
    assert_eq!(status["kitesurf"]["installed"], true);
    assert_eq!(status["kitesurf"]["version"], "v0.5.4");
    assert_eq!(
        status["kitesurf"]["path"],
        install.path().join("bin/kitesurf").display().to_string()
    );
    assert!(
        status["kitesurf"].get("installInstruction").is_none(),
        "an installed kitesurf is not offered for installation"
    );
    let steps = status["nextSteps"].as_array().expect("next steps");
    assert!(
        steps.iter().all(|step| !step
            .as_str()
            .is_some_and(|step| step.contains("install-kitesurf"))),
        "no install step once installed: {steps:?}"
    );
}

#[tokio::test]
async fn connect_probes_the_endpoint_and_page_tools_use_it() {
    let (core, _dir, url, server) = connected_core().await;
    let status = core.status().await;
    assert_eq!(status["connected"], true);
    assert_eq!(status["endpoint"], canonical_url(&url));
    assert_eq!(status["engine"]["version"]["Browser"], "Kitesurf/0.5.4");
    assert_eq!(status["engine"]["pageCount"], 1);
    assert_eq!(
        status["nextSteps"].as_array().map(Vec::len),
        Some(0),
        "a reachable connection needs no next step"
    );
    assert!(status["recommendedEndpoint"]["listening"].is_null());
    server.abort();
}

#[tokio::test]
async fn connect_returns_the_engine_and_canonical_endpoint() {
    let (_, url, server) = fake_engine().await;
    let (core, _dir) = offline_core();
    let value = core.connect(&url).await.expect("reachable");
    assert_eq!(value["connected"], true);
    assert!(value.get("alreadyConnected").is_none());
    assert_eq!(value["endpoint"], canonical_url(&url));
    assert_eq!(value["engine"]["version"]["Browser"], "Kitesurf/0.5.4");
    assert_eq!(value["engine"]["pageCount"], 1);

    // Page tools now reach the endpoint. The fake has no WebSocket, so the
    // failure is a transport/control error, never `not_connected`.
    let error = core
        .new_page(None)
        .await
        .expect_err("the fake has no WebSocket");
    assert_ne!(error.code(), "not_connected", "{error}");
    server.abort();
}

#[tokio::test]
async fn connecting_to_the_same_endpoint_again_is_idempotent() {
    let (core, _dir, url, server) = connected_core().await;
    let before = core.status().await["endpoint"].clone();

    let again = core.connect(&url).await.expect("same endpoint");
    assert_eq!(again["alreadyConnected"], true);
    assert_eq!(again["connected"], true);
    assert_eq!(again["endpoint"], before);
    assert_eq!(core.status().await["endpoint"], before);

    // The ws:// spelling of the same instance is the same canonical endpoint.
    let ws = canonical_url(&url);
    let again = core.connect(&ws).await.expect("canonical ws form");
    assert_eq!(again["alreadyConnected"], true);
    server.abort();
}

#[tokio::test]
async fn a_second_different_endpoint_is_refused_without_switching() {
    let (core, _dir, url, server) = connected_core().await;
    let (_, other_url, other) = fake_engine().await;

    let error = core.connect(&other_url).await.expect_err("no switching");
    assert_eq!(error.code(), "already_connected");
    let rendered = error.to_string();
    assert!(rendered.contains(&canonical_url(&url)), "{rendered}");
    assert!(rendered.contains(&canonical_url(&other_url)), "{rendered}");

    let status = core.status().await;
    assert_eq!(
        status["endpoint"],
        canonical_url(&url),
        "the first endpoint still owns the session"
    );
    assert_eq!(
        status["pageCount"], 0,
        "refusing the switch does not close or create pages"
    );
    other.abort();
    server.abort();
}

#[tokio::test]
async fn an_unreachable_endpoint_is_connect_failed_and_leaves_state_alone() {
    let (core, _dir) = offline_core();
    let error = core
        .connect("http://127.0.0.1:1")
        .await
        .expect_err("nothing listens on port 1");
    assert_eq!(error.code(), "connect_failed");
    assert!(error
        .to_string()
        .contains("ws://127.0.0.1:1/devtools/browser"));
    assert!(core.status().await["endpoint"].is_null(), "no state change");
    assert_eq!(core.endpoint().await.unwrap_err().code(), "not_connected");

    // The same is true once connected: a failed probe never replaces the
    // working endpoint.
    let (core, _dir, _url, server) = connected_core().await;
    let before = core.status().await["endpoint"].clone();
    let error = core
        .connect("http://127.0.0.1:1")
        .await
        .expect_err("the probe fails first");
    assert_eq!(error.code(), "connect_failed");
    assert_eq!(core.status().await["endpoint"], before);
    server.abort();
}

#[tokio::test]
async fn invalid_endpoints_are_rejected_before_any_probe() {
    let (core, _dir) = offline_core();
    for url in [
        "https://127.0.0.1:18971",
        "wss://localhost:18971",
        "http://127.0.0.1",
        "http://example.test:18971",
        "file:///tmp/socket",
    ] {
        let error = core.connect(url).await.expect_err("invalid endpoint");
        assert_eq!(error.code(), "invalid_argument", "{url}");
    }
    assert!(core.status().await["endpoint"].is_null());
}

#[tokio::test]
async fn status_reports_a_connected_but_unreachable_engine() {
    let (core, _dir, _url, server) = connected_core().await;
    server.abort();
    // Give the listener a moment to close before probing again.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let status = core.status().await;
    assert_eq!(status["connected"], false, "reachability is live state");
    assert!(
        status["endpoint"]
            .as_str()
            .is_some_and(|endpoint| endpoint.starts_with("ws://127.0.0.1:")),
        "the configured endpoint is still reported"
    );
    assert_eq!(status["engine"]["reachable"], false);
    assert!(status["engine"]["error"].is_string());
    let steps = status["nextSteps"].as_array().expect("next steps");
    assert!(
        steps.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.starts_with("kitesurf serve --port"))),
        "a dead engine gets a restart hint: {steps:?}"
    );
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
    assert!(
        error
            .hint()
            .is_some_and(|hint| hint.contains("browser_connect")),
        "the hint points at browser_connect"
    );

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
async fn yield_page_needs_a_monitor_gateway() {
    let (core, _dir) = offline_core();
    let error = core.yield_page().await.expect_err("no gateway");
    assert_eq!(error.code(), "gateway_unavailable");
}

#[tokio::test]
async fn yield_page_needs_a_page_once_the_gateway_is_configured() {
    let (_, url, engine) = fake_engine().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(
        Some(dir.path().join("gateway.sock")),
        dir.path().join("provision.lock"),
    ));
    core.connect(&url).await.expect("fake engine");

    let error = core.yield_page().await.expect_err("no page yet");
    assert_eq!(error.code(), "no_active_page");
    engine.abort();
}

#[tokio::test]
async fn request_human_needs_a_connected_gateway() {
    let (core, _dir) = offline_core();
    let error = core
        .request_human("log in", None)
        .await
        .expect_err("no gateway");
    assert_eq!(error.code(), "gateway_unavailable");

    let (_, url, engine) = fake_engine().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let core = BrowserCore::new(config(
        Some(dir.path().join("missing-gateway.sock")),
        dir.path().join("provision.lock"),
    ));
    core.connect(&url).await.expect("fake engine");
    let error = core
        .request_human("log in", Some(100))
        .await
        .expect_err("gateway is not listening");
    assert_eq!(error.code(), "gateway_unavailable");
    engine.abort();
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

#[test]
fn the_install_root_is_part_of_the_config() {
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = config(None, dir.path().join("provision.lock"));
    assert_eq!(
        settings.kitesurf_home,
        Path::new(&std::env::temp_dir()).join("peri-browser-core-test-kitesurf")
    );
}
