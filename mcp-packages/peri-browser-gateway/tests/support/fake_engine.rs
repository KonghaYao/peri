//! 独立 TCP 引擎 fixture：同一 target 上区分 Browser、只读 viewer 和排他 writer。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};
use futures::SinkExt;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub const FRAME: &[u8] = &[0xff, 0xd8, 0xff, 0xd9];
const JPEG: &str = "/9j/2Q==";
const WAIT: Duration = Duration::from_secs(5);

pub struct Engine {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    server: Option<JoinHandle<std::io::Result<()>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Browser,
    Viewer,
    Writer,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Open(Role, String),
    Command(Role, String, String, Value),
    Close(Role, String),
    End(Role, String),
    Conflict(String),
    Probe,
    Enumerate,
}

#[derive(Default)]
struct Target {
    url: String,
    loader: u64,
    writer: bool,
}

#[derive(Default)]
struct Data {
    targets: BTreeMap<String, Target>,
    next_target: u64,
    events: Vec<Event>,
    reply_gate: Option<(String, Arc<Gate>)>,
    close_gate: Option<Arc<Gate>>,
    upgrade_gate: Option<Arc<Gate>>,
}

#[derive(Default)]
struct Shared {
    data: Mutex<Data>,
    changed: Notify,
    stop: CancellationToken,
    tasks: TaskTracker,
}

#[derive(Default)]
pub struct Gate {
    pub arrived: CancellationToken,
    pub release: CancellationToken,
}

impl Gate {
    async fn pass(&self) {
        self.arrived.cancel();
        self.release.cancelled().await;
    }
}

impl Engine {
    pub async fn start(listener: TcpListener) -> anyhow::Result<Self> {
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        let app = Router::new()
            .route("/json/version", get(version))
            .route("/json/list", get(enumeration))
            .route("/devtools/browser", get(browser))
            .route("/devtools/page/{id}", get(page))
            .with_state(shared.clone());
        let stop = shared.stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
        });
        Ok(Self {
            addr,
            shared,
            server: Some(server),
        })
    }

    pub async fn ephemeral() -> anyhow::Result<Self> {
        Self::start(TcpListener::bind("127.0.0.1:0").await?).await
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn events(&self) -> Vec<Event> {
        self.shared.data.lock().unwrap().events.clone()
    }

    pub fn targets(&self) -> Vec<String> {
        self.shared
            .data
            .lock()
            .unwrap()
            .targets
            .keys()
            .cloned()
            .collect()
    }

    pub async fn wait(&self, predicate: impl Fn(&[Event]) -> bool) -> anyhow::Result<()> {
        tokio::time::timeout(WAIT, async {
            loop {
                let changed = self.shared.changed.notified();
                if predicate(&self.shared.data.lock().unwrap().events) {
                    break;
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("等待 fixture 引擎事件超时"))
    }

    pub fn gate_reply(&self, method: &str) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.shared.data.lock().unwrap().reply_gate = Some((method.to_owned(), gate.clone()));
        gate
    }

    pub fn gate_close(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.shared.data.lock().unwrap().close_gate = Some(gate.clone());
        gate
    }

    pub fn gate_upgrade(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.shared.data.lock().unwrap().upgrade_gate = Some(gate.clone());
        gate
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.shared.stop.cancel();
        self.shared.tasks.close();
        if let Some(mut server) = self.server.take() {
            tokio::time::timeout(WAIT, &mut server).await???;
        }
        tokio::time::timeout(WAIT, self.shared.tasks.wait()).await?;
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop.cancel();
    }
}

impl Shared {
    fn record(&self, event: Event) {
        self.data.lock().unwrap().events.push(event);
        self.changed.notify_waiters();
    }
}

async fn version(State(shared): State<Arc<Shared>>) -> Json<Value> {
    shared.record(Event::Probe);
    Json(
        json!({"Browser": "Kitesurf/fixture", "Protocol-Version": "1.3", "webSocketDebuggerUrl": "ws://not-used.invalid"}),
    )
}

async fn enumeration(State(shared): State<Arc<Shared>>) -> StatusCode {
    shared.record(Event::Enumerate);
    StatusCode::NOT_FOUND
}

async fn browser(State(shared): State<Arc<Shared>>, upgrade: WebSocketUpgrade) -> Response {
    socket(shared, upgrade, Role::Browser, String::new())
}

async fn page(
    State(shared): State<Arc<Shared>>,
    Path(id): Path<String>,
    uri: Uri,
    upgrade: WebSocketUpgrade,
) -> Response {
    let role = if uri
        .query()
        .is_some_and(|query| query.split('&').any(|q| q == "role=viewer"))
    {
        Role::Viewer
    } else {
        Role::Writer
    };
    let gate = if role == Role::Writer {
        shared.data.lock().unwrap().upgrade_gate.take()
    } else {
        None
    };
    if let Some(gate) = gate {
        tokio::select! { _ = shared.stop.cancelled() => return StatusCode::SERVICE_UNAVAILABLE.into_response(), _ = gate.pass() => {} }
    }
    {
        let mut data = shared.data.lock().unwrap();
        let Some(target) = data.targets.get_mut(&id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if role == Role::Writer {
            if target.writer {
                drop(data);
                shared.record(Event::Conflict(id));
                return StatusCode::CONFLICT.into_response();
            }
            target.writer = true;
        }
    }
    socket(shared, upgrade, role, id)
}

struct SocketGuard {
    shared: Arc<Shared>,
    role: Role,
    id: String,
    released: bool,
    opened: bool,
}

impl SocketGuard {
    fn release(&mut self) {
        if self.role == Role::Writer && !self.released {
            if let Some(target) = self.shared.data.lock().unwrap().targets.get_mut(&self.id) {
                target.writer = false;
            }
        }
        self.released = true;
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.release();
        if self.opened {
            self.shared.record(Event::End(self.role, self.id.clone()));
        }
    }
}

fn socket(shared: Arc<Shared>, upgrade: WebSocketUpgrade, role: Role, id: String) -> Response {
    let ticket = shared.tasks.token();
    let guard = SocketGuard {
        shared,
        role,
        id,
        released: false,
        opened: false,
    };
    upgrade.on_upgrade(move |socket| async move {
        let _ticket = ticket;
        drive(socket, guard).await;
    })
}

async fn drive(mut socket: WebSocket, mut guard: SocketGuard) {
    let shared = guard.shared.clone();
    let (role, id) = (guard.role, guard.id.clone());
    guard.opened = true;
    shared.record(Event::Open(role, id.clone()));
    let mut frame = 0;
    loop {
        let message = tokio::select! { _ = shared.stop.cancelled() => break, message = socket.recv() => message };
        match message {
            Some(Ok(Message::Close(_))) => {
                shared.record(Event::Close(role, id.clone()));
                let gate = if role == Role::Writer {
                    shared.data.lock().unwrap().close_gate.take()
                } else {
                    None
                };
                if let Some(gate) = gate {
                    tokio::select! { _ = shared.stop.cancelled() => break, _ = gate.pass() => {} }
                }
                guard.release();
                // tungstenite 在读 Close 时排队了应答；继续发送会 flush 它。
                let _ = socket.flush().await;
                break;
            }
            Some(Ok(Message::Text(text))) => {
                let request: Value = serde_json::from_str(text.as_str()).unwrap();
                let method = request["method"].as_str().unwrap();
                let params = &request["params"];
                shared.record(Event::Command(
                    role,
                    id.clone(),
                    method.to_owned(),
                    params.clone(),
                ));
                let gate = {
                    let mut data = shared.data.lock().unwrap();
                    if data.reply_gate.as_ref().is_some_and(|(m, _)| m == method) {
                        data.reply_gate.take().map(|(_, gate)| gate)
                    } else {
                        None
                    }
                };
                if let Some(gate) = gate {
                    // 继续读 Close；即使旧 CDP reply 在途，关闭也不能依赖它。
                    tokio::select! {
                        _ = shared.stop.cancelled() => break,
                        incoming = socket.recv() => {
                            if matches!(incoming, Some(Ok(Message::Close(_)))) {
                                shared.record(Event::Close(role, id.clone()));
                                guard.release();
                                let _ = socket.flush().await;
                            }
                            break;
                        }
                        _ = gate.pass() => {}
                    }
                }
                let response = match reply(&shared, role, &id, method, params) {
                    Ok(_)
                        if method == "Runtime.evaluate"
                            && params["expression"].as_str().is_some_and(|s| {
                                s.starts_with("location.assign(")
                                    && s.contains("/fixture-committed-error?")
                            }) =>
                    {
                        json!({"id": request["id"], "error": {"code": -32000, "message": params["expression"]}})
                    }
                    Ok(result) => json!({"id": request["id"], "result": result}),
                    Err(error) => {
                        json!({"id": request["id"], "error": {"code": -32000, "message": error}})
                    }
                };
                if socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
                if role == Role::Viewer
                    && matches!(method, "Page.startScreencast" | "Page.screencastFrameAck")
                {
                    // 模拟连续画面，避免 fixture 瞬间耗尽所有帧或忙循环。
                    tokio::time::sleep(Duration::from_millis(16)).await;
                    frame += 1;
                    let event = json!({"method": "Page.screencastFrame", "params": {"data": JPEG, "sessionId": frame,
                        "metadata": {"deviceWidth": 1280, "deviceHeight": 960, "pageScaleFactor": 2, "scrollOffsetX": 0, "scrollOffsetY": 9}}});
                    if socket
                        .send(Message::Text(event.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
            None | Some(Err(_)) => break,
            _ => {}
        }
    }
}

fn reply(
    shared: &Shared,
    role: Role,
    id: &str,
    method: &str,
    params: &Value,
) -> Result<Value, &'static str> {
    if role == Role::Viewer
        && !matches!(
            method,
            "Page.getLayoutMetrics"
                | "Page.startScreencast"
                | "Page.screencastFrameAck"
                | "Page.captureScreenshot"
        )
    {
        return Err("Viewer connection cannot control the browser");
    }
    let mut data = shared.data.lock().unwrap();
    if role == Role::Browser {
        return match method {
            "Target.createTarget" => {
                data.next_target += 1;
                let id = format!("target-{}", data.next_target);
                data.targets.insert(
                    id.clone(),
                    Target {
                        url: "about:blank".to_owned(),
                        loader: 1,
                        writer: false,
                    },
                );
                Ok(json!({"targetId": id}))
            }
            "Target.closeTarget" => Ok(
                json!({"success": params["targetId"].as_str().and_then(|id| data.targets.remove(id)).is_some()}),
            ),
            _ => Err("仅接受实例级建页和关页"),
        };
    }
    let target = data.targets.get_mut(id).ok_or("Target expired")?;
    Ok(match method {
        "Runtime.evaluate" => {
            let expression = params["expression"].as_str().unwrap_or_default();
            let value = match expression {
                "fixture_ack" => json!("fixture_ack"),
                "location.href" => json!(target.url),
                "document.title" => json!("Fixture"),
                "document.readyState" => json!("complete"),
                _ => {
                    if let Some(url) = expression
                        .strip_prefix("location.assign(")
                        .and_then(|s| s.strip_suffix(')'))
                    {
                        target.url = serde_json::from_str(url).unwrap();
                        target.loader += 1;
                    }
                    json!(true)
                }
            };
            json!({"result": {"value": value}})
        }
        "Page.getFrameTree" => {
            json!({"frameTree": {"frame": {"id": "root", "loaderId": target.loader.to_string()}}})
        }
        "Page.getNavigationHistory" => {
            json!({"currentIndex": 0, "entries": [{"id": 1, "title": "Fixture", "url": target.url}]})
        }
        "Accessibility.getFullAXTree" => {
            json!({"nodes": [{"nodeId": "1", "role": {"value": "button"}, "name": {"value": "Fixture"}, "backendDOMNodeId": 42}]})
        }
        "DOM.getBoxModel" => json!({"model": {"content": [10, 10, 30, 10, 30, 20, 10, 20]}}),
        "Page.getLayoutMetrics" => {
            json!({"cssLayoutViewport": {"clientWidth": 640, "clientHeight": 480}})
        }
        "Page.captureScreenshot" => json!({"data": JPEG}),
        _ => json!({}),
    })
}
