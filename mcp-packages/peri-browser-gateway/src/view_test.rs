//! view session 契约：token、HTML 上限、会话生命周期、SSE 帧与请求、真实输入、
//! 结果回填与关闭语义。假引擎按脚本应答 CDP，不需要真实浏览器。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use super::api::{bearer, ClickBody};
use super::*;
use crate::apps::{AppDocument, AppDocuments};

const TOKEN: &str = "test-view-token";

struct Fixture {
    base: String,
    http: reqwest::Client,
    stop: CancellationToken,
    server: JoinHandle<anyhow::Result<()>>,
    engine: Option<FakeEngine>,
    _home: tempfile::TempDir,
}

impl Fixture {
    async fn start(with_engine: bool, token: Option<&str>) -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let engine = if with_engine {
            Some(FakeEngine::start().await?)
        } else {
            None
        };
        let cdp_url = engine.as_ref().map_or_else(
            || "http://127.0.0.1:9".to_owned(),
            |engine| engine.url.clone(),
        );
        let lock = home.path().join("provision.lock");
        let env_home = home.path().to_string_lossy().into_owned();
        let env_lock = lock.to_string_lossy().into_owned();
        let token = token.map(str::to_owned);
        let config = Config::parse_from(["--cdp-url".to_owned(), cdp_url], move |key| match key {
            "HOME" => Some(env_home.clone()),
            "PERI_BROWSER_PROVISION_LOCK" => Some(env_lock.clone()),
            "PERI_BROWSER_CONNECT_TIMEOUT_MS" => Some("300".to_owned()),
            "PERI_BROWSER_PAGE_WAIT_MS" => Some("3000".to_owned()),
            "PERI_BROWSER_VIEW_TOKEN" => token.clone(),
            _ => None,
        })?;
        let documents = Arc::new(AppDocuments::new(AppDocument::placeholder()));
        let views = ViewSessions::new(&config, documents);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let stop = CancellationToken::new();
        let server = tokio::spawn(crate::serve(
            listener,
            config,
            views,
            stop.clone().cancelled_owned(),
        ));
        Ok(Self {
            base,
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()?,
            stop,
            server,
            engine,
            _home: home,
        })
    }

    async fn create(&self, html: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/view/sessions", self.base))
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(html.to_owned())
            .send()
            .await
            .expect("create request")
    }

    async fn status(&self, id: &str) -> Value {
        self.http
            .get(format!("{}/view/sessions/{id}", self.base))
            .header("authorization", format!("Bearer {TOKEN}"))
            .send()
            .await
            .expect("status request")
            .json()
            .await
            .expect("status body")
    }

    async fn wait_ready(&self, id: &str) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            let status = self.status(id).await;
            if status["state"] == "ready" || status["state"] == "failed" {
                return status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "session did not leave starting: {status}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn finish(self) {
        self.stop.cancel();
        let _ = self.server.await;
        if let Some(engine) = self.engine {
            engine.task.abort();
        }
    }
}

#[tokio::test]
async fn view_api_requires_the_configured_token() -> anyhow::Result<()> {
    let fixture = Fixture::start(false, Some(TOKEN)).await?;
    let missing = fixture
        .http
        .post(format!("{}/view/sessions", fixture.base))
        .body("<html><head></head></html>")
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let wrong = fixture
        .http
        .post(format!("{}/view/sessions", fixture.base))
        .header("authorization", "Bearer nope")
        .body("<html><head></head></html>")
        .send()
        .await?;
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);
    let header = fixture
        .http
        .get(format!("{}/view/sessions", fixture.base))
        .header("x-peri-view-token", TOKEN)
        .send()
        .await?;
    assert_eq!(header.status(), StatusCode::OK);
    fixture.finish().await;
    Ok(())
}

#[tokio::test]
async fn unconfigured_token_disables_the_view_api() -> anyhow::Result<()> {
    let fixture = Fixture::start(false, None).await?;
    let response = fixture
        .http
        .post(format!("{}/view/sessions", fixture.base))
        .body("<html><head></head></html>")
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.text().await?.contains("view_api_disabled"));
    fixture.finish().await;
    Ok(())
}

#[tokio::test]
async fn html_size_and_encoding_are_validated() -> anyhow::Result<()> {
    let fixture = Fixture::start(false, Some(TOKEN)).await?;
    let empty = fixture.create("").await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    let oversized = fixture
        .http
        .post(format!("{}/view/sessions", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(vec![b'a'; MAX_APP_HTML_BYTES + 1])
        .send()
        .await?;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let invalid = fixture
        .http
        .post(format!("{}/view/sessions", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(vec![0xff_u8, 0xfe])
        .send()
        .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    // 上限以内必须接受：axum 默认 2 MiB 的 body 限制不能截断 5 MiB 的 App HTML。
    let mut html = String::with_capacity(MAX_APP_HTML_BYTES + 32);
    html.push_str("<html><head></head><body>");
    html.push_str(&"a".repeat(MAX_APP_HTML_BYTES - 64));
    html.push_str("</body></html>");
    let accepted = fixture.create(&html).await;
    assert_eq!(accepted.status(), StatusCode::CREATED);
    fixture.finish().await;
    Ok(())
}

#[tokio::test]
async fn session_streams_frames_relays_tool_calls_and_closes() -> anyhow::Result<()> {
    let fixture = Fixture::start(true, Some(TOKEN)).await?;
    let created = fixture
        .create("<html><head></head><body>app</body></html>")
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await?;
    let id = created["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    assert_eq!(
        created["url"],
        format!("http://127.0.0.1:7392/apps/view?view={id}")
    );
    let status = fixture.wait_ready(&id).await;
    assert_eq!(status["state"], "ready", "{status}");

    // SSE: meta、第一帧与 App 的 tools/call 请求都要到达宿主。
    let mut events = SseClient::open(&fixture.base, &id).await?;
    events.wait_for("event: meta").await?;
    let frame = events.wait_for("\"viewportWidth\"").await?;
    assert!(
        frame.contains("\"viewportWidth\":800"),
        "帧视口必须取 startScreencast 之后的值: {frame}"
    );
    let request = events.wait_for("\"method\":\"tools/call\"").await?;
    assert!(request.contains("\"seq\":1"), "{request}");

    let status = fixture.status(&id).await;
    assert_eq!(status["pendingRequests"], 1, "{status}");

    // 重连语义：未回答的请求在第二条流上重放，而不是只发给第一个订阅者。
    let mut replay = SseClient::open(&fixture.base, &id).await?;
    replay.wait_for("event: meta").await?;
    let replayed = replay.wait_for("\"method\":\"tools/call\"").await?;
    assert!(replayed.contains("\"seq\":1"), "{replayed}");

    let notified = fixture
        .http
        .post(format!("{}/view/sessions/{id}/notify", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .json(&json!({
            "method": "ui/notifications/tool-input",
            "params": {"arguments": {"city": "Shanghai"}}
        }))
        .send()
        .await?;
    assert_eq!(notified.status(), StatusCode::OK);
    let engine = fixture.engine.as_ref().expect("fake engine");
    assert!(engine.commands().iter().any(|command| {
        command["method"] == "Runtime.evaluate"
            && command["params"]["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains("hostNotify"))
    }));

    // 真实 CDP 输入：move → press → release，坐标原样下发。
    let clicked = fixture
        .http
        .post(format!("{}/view/sessions/{id}/click", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .json(&json!({"x": 20.0, "y": 30.0}))
        .send()
        .await?;
    assert_eq!(clicked.status(), StatusCode::OK);
    let engine = fixture.engine.as_ref().expect("fake engine");
    let mouse = engine.mouse_events();
    assert_eq!(mouse, ["mouseMoved", "mousePressed", "mouseReleased"]);
    assert!(engine
        .commands()
        .iter()
        .any(|command| command["method"] == "Input.dispatchMouseEvent"
            && command["params"]["x"] == 20.0
            && command["params"]["y"] == 30.0));

    // 宿主结果回填：页面收到 hostReply，请求出队。
    let answered = fixture
        .http
        .post(format!("{}/view/sessions/{id}/app-results", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .json(&json!({
            "seq": 1,
            "result": {"content": [{"type": "text", "text": "2030-01-01T00:00:00Z"}]}
        }))
        .send()
        .await?;
    assert_eq!(answered.status(), StatusCode::OK);
    assert!(engine.commands().iter().any(|command| {
        command["method"] == "Runtime.evaluate"
            && command["params"]["expression"]
                .as_str()
                .is_some_and(|expression| {
                    expression.contains("hostReply") && expression.contains("\"id\":5")
                })
    }));
    assert_eq!(fixture.status(&id).await["pendingRequests"], 0);

    let evaluated = fixture
        .http
        .post(format!("{}/view/sessions/{id}/evaluate", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .json(&json!({ "expression": "1 + 1" }))
        .send()
        .await?;
    assert_eq!(evaluated.status(), StatusCode::OK);
    assert_eq!(evaluated.json::<Value>().await?["result"], 7);

    // 关闭：SSE 收到 closed，记录随后返回 404。
    let deleted = fixture
        .http
        .delete(format!("{}/view/sessions/{id}", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await?;
    assert_eq!(deleted.status(), StatusCode::OK);
    events.wait_for("event: closed").await?;
    let gone = fixture
        .http
        .get(format!("{}/view/sessions/{id}", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await?;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    fixture.finish().await;
    Ok(())
}

#[tokio::test]
async fn unknown_session_and_bad_bodies_are_rejected() -> anyhow::Result<()> {
    let fixture = Fixture::start(false, Some(TOKEN)).await?;
    let missing = fixture
        .http
        .get(format!("{}/view/sessions/nope", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let unknown = fixture
        .http
        .post(format!("{}/view/sessions/nope/click", fixture.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .json(&json!({"x": 1.0, "y": 2.0}))
        .send()
        .await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    fixture.finish().await;
    Ok(())
}

#[test]
fn bearer_accepts_both_headers_and_rejects_other_schemes() {
    use axum::http::HeaderValue;

    let mut headers = axum::http::HeaderMap::new();
    assert_eq!(bearer(&headers), None);
    headers.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer abc"),
    );
    assert_eq!(bearer(&headers), Some("abc"));
    headers.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_static("Basic abc"),
    );
    assert_eq!(bearer(&headers), None);
    headers.insert("x-peri-view-token", HeaderValue::from_static("xyz"));
    assert_eq!(bearer(&headers), Some("xyz"));
}

#[test]
fn click_body_requires_finite_viewport_points() {
    let ok = ClickBody {
        action: None,
        x: 12.0,
        y: 8.5,
        delta_x: 0.0,
        delta_y: 0.0,
    };
    assert_eq!(ok.point().expect("valid point"), (12.0, 8.5));
    for (x, y) in [(f64::NAN, 0.0), (0.0, f64::INFINITY), (200_000.0, 0.0)] {
        let bad = ClickBody {
            action: Some("click".to_owned()),
            x,
            y,
            delta_x: 0.0,
            delta_y: 0.0,
        };
        assert!(bad.point().is_err(), "{x},{y} must be rejected");
    }
}

#[tokio::test]
async fn session_limit_is_enforced_and_released_by_delete() -> anyhow::Result<()> {
    let fixture = Fixture::start(true, Some(TOKEN)).await?;
    let mut created = Vec::new();
    for index in 0..MAX_SESSIONS {
        let response = fixture
            .create(&format!(
                "<html><head></head><body>app {index}</body></html>"
            ))
            .await;
        assert_eq!(response.status(), StatusCode::CREATED, "session {index}");
        let value: Value = response.json().await?;
        created.push(value["sessionId"].as_str().expect("session id").to_owned());
    }
    let rejected = fixture
        .create("<html><head></head><body>overflow</body></html>")
        .await;
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(rejected.text().await?.contains("session_limit"));

    let closed = fixture
        .http
        .delete(format!("{}/view/sessions/{}", fixture.base, created[0]))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await?;
    assert_eq!(closed.status(), StatusCode::OK);
    for id in &created[1..] {
        fixture
            .http
            .delete(format!("{}/view/sessions/{id}", fixture.base))
            .header("authorization", format!("Bearer {TOKEN}"))
            .send()
            .await?;
    }
    let accepted = fixture
        .create("<html><head></head><body>replacement</body></html>")
        .await;
    assert_eq!(accepted.status(), StatusCode::CREATED);
    fixture.finish().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// SSE client: the gateway streams chunked HTTP; assertions read raw bytes.
// ---------------------------------------------------------------------------

struct SseClient {
    stream: TcpStream,
    buffer: String,
}

impl SseClient {
    async fn open(base: &str, id: &str) -> anyhow::Result<Self> {
        let address = base.trim_start_matches("http://");
        let mut stream = TcpStream::connect(address).await?;
        let request = format!(
            "GET /view/sessions/{id}/events HTTP/1.1\r\nHost: {address}\r\nAccept: text/event-stream\r\nAuthorization: Bearer {TOKEN}\r\nConnection: keep-alive\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        Ok(Self {
            stream,
            buffer: String::new(),
        })
    }

    async fn wait_for(&mut self, needle: &str) -> anyhow::Result<String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(found) = self.buffer.find(needle) {
                let start = self.buffer[..found]
                    .rfind('\n')
                    .map_or(0, |index| index + 1);
                // 只返回收全整行的匹配：TCP 分片可能在命中位置后立即结束，
                // 此时 `viewportWidth` 的值还在下一段字节里。
                if let Some(index) = self.buffer[found..].find('\n') {
                    return Ok(self.buffer[start..found + index].to_owned());
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            anyhow::ensure!(!remaining.is_zero(), "timed out waiting for {needle:?}");
            let mut chunk = [0_u8; 8192];
            let read = tokio::time::timeout(remaining, self.stream.read(&mut chunk)).await;
            match read {
                Ok(Ok(0)) => anyhow::bail!("SSE stream closed before {needle:?}"),
                Ok(Ok(read)) => {
                    self.buffer
                        .push_str(&String::from_utf8_lossy(&chunk[..read]));
                }
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => anyhow::bail!("timed out waiting for {needle:?}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fake Kitesurf: one port, `Target.createTarget`, page controller and viewer.
// ---------------------------------------------------------------------------

struct FakeEngine {
    url: String,
    commands: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl FakeEngine {
    async fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let commands = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&commands);
        let connections = Arc::new(Mutex::new(Vec::new()));
        let handles = Arc::clone(&connections);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                handles
                    .lock()
                    .expect("connections lock")
                    .push(tokio::spawn(serve_fake(stream, Arc::clone(&seen))));
            }
        });
        Ok(Self {
            url,
            commands,
            task,
            connections,
        })
    }

    /// 模拟浏览器进程退出：关闭已接受的 CDP 连接（含 writer controller）。
    fn abort_connections(&self) {
        for handle in self.connections.lock().expect("connections lock").drain(..) {
            handle.abort();
        }
    }

    fn commands(&self) -> Vec<Value> {
        self.commands.lock().expect("commands lock").clone()
    }

    fn mouse_events(&self) -> Vec<String> {
        self.commands()
            .iter()
            .filter(|command| command["method"] == "Input.dispatchMouseEvent")
            .filter_map(|command| command["params"]["type"].as_str().map(str::to_owned))
            .collect()
    }
}

#[derive(Default)]
struct PageState {
    url: String,
    drained: bool,
    /// `Page.startScreencast` 会改变布局视口；假引擎据此模拟前后不同的度量。
    screencast_started: bool,
}

enum Role {
    Browser,
    Controller,
    Viewer,
}

async fn serve_fake(stream: TcpStream, seen: Arc<Mutex<Vec<Value>>>) {
    let mut prefix = [0_u8; 2048];
    let peeked = stream.peek(&mut prefix).await.unwrap_or(0);
    let head = String::from_utf8_lossy(&prefix[..peeked]).to_ascii_lowercase();
    if head.starts_with("get ") && !head.contains("upgrade: websocket") {
        return;
    }
    let role = if head.contains("role=viewer") {
        Role::Viewer
    } else if head.contains("/devtools/browser") {
        Role::Browser
    } else {
        Role::Controller
    };
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let mut state = PageState {
        url: "about:blank".to_owned(),
        drained: false,
        screencast_started: false,
    };
    while let Some(message) = socket.next().await {
        let text = match message {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => {
                let _ = socket.flush().await;
                break;
            }
            _ => break,
        };
        let Ok(request) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        seen.lock().expect("commands lock").push(request.clone());
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let result = fake_reply(method, &params, &mut state);
        if socket
            .send(Message::Text(
                json!({"id": id, "result": result}).to_string().into(),
            ))
            .await
            .is_err()
        {
            break;
        }
        if matches!(role, Role::Controller) && method == "Runtime.enable" {
            let wake = json!({
                "method": "Runtime.consoleAPICalled",
                "params": {
                    "type": "debug",
                    "args": [{"type": "string", "value": "peri-view:app-request 1"}],
                }
            });
            let _ = socket.send(Message::Text(wake.to_string().into())).await;
        }
        if matches!(role, Role::Viewer) && method == "Page.startScreencast" {
            let frame = json!({
                "method": "Page.screencastFrame",
                "params": {
                    "data": "aGVsbG8=",
                    "metadata": {
                        "deviceWidth": 800,
                        "deviceHeight": 600,
                        "pageScaleFactor": 1.0,
                        "scrollOffsetX": 0.0,
                        "scrollOffsetY": 0.0
                    },
                    "sessionId": 1
                }
            });
            let _ = socket.send(Message::Text(frame.to_string().into())).await;
        }
    }
}

fn fake_reply(method: &str, params: &Value, state: &mut PageState) -> Value {
    match method {
        "Target.createTarget" => json!({"targetId": "view-target-1"}),
        "Runtime.evaluate" => {
            let expression = params
                .get("expression")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if expression == "location.href" {
                return json!({"result": {"type": "string", "value": state.url}});
            }
            if expression == "document.title" {
                return json!({"result": {"type": "string", "value": "View"}});
            }
            if expression == "document.readyState" {
                return json!({"result": {"type": "string", "value": "complete"}});
            }
            if let Some(assigned) = expression
                .strip_prefix("location.assign(")
                .and_then(|rest| rest.strip_suffix(')'))
            {
                if let Ok(url) = serde_json::from_str::<String>(assigned) {
                    state.url = url;
                }
                return json!({"result": {"type": "object"}});
            }
            if expression.contains("takeRequests") {
                let items = if state.drained {
                    json!([])
                } else {
                    state.drained = true;
                    json!([{
                        "seq": 1,
                        "id": 5,
                        "method": "tools/call",
                        "params": {"name": "get-time", "arguments": {}}
                    }])
                };
                return json!({"result": {"type": "object", "value": items}});
            }
            if expression.contains("hostReply") || expression.contains("hostNotify") {
                return json!({"result": {"type": "boolean", "value": true}});
            }
            json!({"result": {"type": "number", "value": 7}})
        }
        "Page.getNavigationHistory" => json!({
            "currentIndex": 0,
            "entries": [{"id": 1, "url": state.url, "title": "View"}],
        }),
        "Page.getFrameTree" => {
            json!({"frameTree": {"frame": {"id": "top", "loaderId": state.url}}})
        }
        "Page.getLayoutMetrics" => {
            // 启动 screencast 前 1280x720，启动后（maxWidth/Height 生效）800x600。
            let (width, height) = if state.screencast_started {
                (800, 600)
            } else {
                (1280, 720)
            };
            json!({"cssLayoutViewport": {"clientWidth": width, "clientHeight": height}})
        }
        "Page.startScreencast" => {
            state.screencast_started = true;
            json!({})
        }
        _ => json!({}),
    }
}

/// A5：控制器在中途断开时，失败码必须在 teardown 之后仍然可见。
///
/// 真实链路里 `fail(controller_closed)` 与 `finish()` 紧邻；若 `finish` 直接
/// 覆盖为 `Closed`，SSE watch 只观察到终态，宿主只能看到无原因的 `closed`。
#[test]
fn failed_session_keeps_failure_across_teardown() {
    let documents = Arc::new(AppDocuments::new(AppDocument::placeholder()));
    let session = ViewSession::new(
        "failed-session".to_owned(),
        "http://127.0.0.1/apps".to_owned(),
        CancellationToken::new(),
        documents,
    );
    session.fail("controller_closed", "the page controller socket closed");
    session.finish();
    let status = session.status();
    assert_eq!(status["state"], "failed");
    assert_eq!(status["code"], "controller_closed");
    assert_eq!(
        status["message"], "the page controller socket closed",
        "失败原因不能退化为无原因的 closed"
    );
}

/// 正常关闭仍落到 `Closed`。
#[test]
fn clean_session_finishes_as_closed() {
    let documents = Arc::new(AppDocuments::new(AppDocument::placeholder()));
    let session = ViewSession::new(
        "clean-session".to_owned(),
        "http://127.0.0.1/apps".to_owned(),
        CancellationToken::new(),
        documents,
    );
    session.finish();
    assert_eq!(session.status()["state"], "closed");
}

/// A5：浏览器（Kitesurf）中途退出时，宿主必须收到带失败码的 `state failed`，
/// 而不是只有无原因的 `closed`。
///
/// teardown 先 cancel stop 再 finish；SSE pump 的 biased select 会先观察到
/// stop，若直接退出就会把失败阶段丢掉。
#[tokio::test]
async fn engine_exit_reports_failed_before_closed() -> anyhow::Result<()> {
    let fixture = Fixture::start(true, Some(TOKEN)).await?;
    let created = fixture
        .create("<html><head></head><body>app</body></html>")
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await?;
    let id = created["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    assert_eq!(fixture.wait_ready(&id).await["state"], "ready");

    let mut events = SseClient::open(&fixture.base, &id).await?;
    events.wait_for("event: meta").await?;
    // 模拟浏览器进程退出：CDP 连接全部断开。
    let engine = fixture.engine.as_ref().expect("fake engine");
    engine.abort_connections();

    let failed = events.wait_for("\"state\":\"failed\"").await?;
    assert!(failed.contains("controller_closed"), "{failed}");
    events.wait_for("event: closed").await?;
    fixture.finish().await;
    Ok(())
}
