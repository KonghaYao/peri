use std::sync::Arc;

use reqwest::Client;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;
use crate::protocol::{AgentState, PageRef};
use crate::registry::RegistryHandle;

struct TestServer {
    base: String,
    sessions: Arc<SessionStore>,
    task: JoinHandle<()>,
}

impl TestServer {
    async fn start() -> Self {
        let sessions = Arc::new(SessionStore::for_tests(b"secret".to_vec()));
        let state = AppState {
            sessions: sessions.clone(),
            registry: Arc::new(RegistryHandle::new()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        let app = build_router(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        Self {
            base: format!("http://{address}"),
            sessions,
            task,
        }
    }

    async fn register(&self, name: &str) -> crate::session::Session {
        self.sessions
            .register(
                "s1",
                name,
                "ws://127.0.0.1:8827",
                vec![PageRef {
                    target_id: "t1".to_owned(),
                    url: "about:blank".to_owned(),
                    title: "New Tab".to_owned(),
                }],
                AgentState::Idle,
            )
            .await
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn healthz_reports_the_session_count() {
    let server = TestServer::start().await;
    server.register("peri-abc").await;

    let response = Client::new()
        .get(format!("{}/healthz", server.base))
        .send()
        .await
        .expect("health request");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body, serde_json::json!({"ok": true, "sessions": 1}));
}

#[tokio::test]
async fn index_lists_sessions_and_escapes_names() {
    let server = TestServer::start().await;
    let session = server.register("<script>alert(1)</script>").await;

    let response = Client::new()
        .get(format!("{}/", server.base))
        .send()
        .await
        .expect("index request");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    let body = response.text().await.expect("html body");
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!body.contains("<script>alert(1)</script>"));
    assert!(body.contains(&format!("/s/{}/", session.token)));
    assert!(body.contains("1 page(s)"));
    assert!(body.contains("idle"));
}

#[tokio::test]
async fn index_without_sessions_says_so() {
    let server = TestServer::start().await;
    let body = Client::new()
        .get(format!("{}/", server.base))
        .send()
        .await
        .expect("index request")
        .text()
        .await
        .expect("html body");
    assert!(body.contains("no sessions"));
    assert!(body.contains("Sessions (0)"));
}

#[tokio::test]
async fn monitor_page_is_served_with_safe_headers() {
    let server = TestServer::start().await;
    let session = server.register("peri-abc").await;

    let response = Client::new()
        .get(format!("{}/s/{}/", server.base, session.token))
        .send()
        .await
        .expect("monitor request");
    assert_eq!(response.status(), 200);

    let headers = response.headers();
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        headers
            .get("referrer-policy")
            .and_then(|value| value.to_str().ok()),
        Some("no-referrer")
    );
    assert_eq!(
        headers
            .get("x-frame-options")
            .and_then(|value| value.to_str().ok()),
        Some("DENY")
    );

    let body = response.text().await.expect("html body");
    assert!(body.contains("peri-browser monitor"));
    assert!(body.contains("/ws"));
}

#[tokio::test]
async fn unknown_tokens_are_404_for_page_and_socket() {
    let server = TestServer::start().await;
    server.register("peri-abc").await;

    for path in ["/s/nope/", "/s/nope/ws"] {
        let response = Client::new()
            .get(format!("{}{path}", server.base))
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), 404, "{path} must be 404");
    }
}

#[tokio::test]
async fn known_socket_path_without_upgrade_is_400() {
    let server = TestServer::start().await;
    let session = server.register("peri-abc").await;

    let response = Client::new()
        .get(format!("{}/s/{}/ws", server.base, session.token))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 400);
}

#[test]
fn escape_html_covers_the_dangerous_characters() {
    assert_eq!(
        escape_html(r#"<a href="x">'&'"#),
        "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;"
    );
    assert_eq!(escape_html("plain"), "plain");
}

/// The monitor page is the human half of the contract (README §6.3, §6.8).
/// The asset is compiled in, so these checks guard the shape the gateway's
/// messages are written against: separate takeover/release actions, a handoff
/// banner that echoes the gateway's request id, a diagnostics drawer, and
/// pointer coordinates left in canvas pixels for the gateway to map.
#[test]
fn monitor_page_exposes_the_human_control_surface() {
    for needle in [
        "id=\"takeover\"",
        "id=\"release\"",
        "id=\"handoffDone\"",
        "requestId: state.handoff.requestId",
        "id=\"drawer\"",
        "diagnosticsPage(message)",
    ] {
        assert!(
            MONITOR_HTML.contains(needle),
            "monitor.html must contain {needle}"
        );
    }
    assert!(
        !MONITOR_HTML.contains("requestId: \"current\""),
        "the handoff answer must echo the gateway's request id"
    );
    assert!(
        !MONITOR_HTML.contains("meta.viewportWidth"),
        "pointer coordinates stay in canvas pixels; the gateway maps them to CSS"
    );
}
