//! 一个任务驱动 WebSocket 的收发。关闭信号独立于有界命令和事件队列。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use super::{CdpError, CdpEvent, COMMAND_TIMEOUT, EVENT_CAPACITY};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Reply = oneshot::Sender<Result<Value, CdpError>>;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PENDING: usize = 32;

struct Command {
    id: u64,
    payload: Value,
    reply: Reply,
}

pub(super) struct Connection {
    url: String,
    commands: mpsc::Sender<Command>,
    events: Option<mpsc::Receiver<CdpEvent>>,
    next_id: AtomicU64,
    in_flight: Arc<AtomicUsize>,
    stop: CancellationToken,
    closed: CancellationToken,
    task: Option<JoinHandle<bool>>,
    close_confirmed: Option<bool>,
}

impl Connection {
    pub async fn open(url: String, timeout: Duration, controller: bool) -> Result<Self, CdpError> {
        let (socket, _) = tokio::time::timeout(timeout, tokio_tungstenite::connect_async(&url))
            .await
            .map_err(|_| CdpError::Timeout {
                method: "WebSocket connect".to_owned(),
                timeout_ms: timeout.as_millis() as u64,
            })?
            .map_err(|source| match &source {
                WsError::Http(response) if controller && response.status().as_u16() == 409 => {
                    CdpError::AnotherController
                }
                _ => CdpError::Connect {
                    url: url.clone(),
                    source,
                },
            })?;
        let (commands, rx) = mpsc::channel(MAX_PENDING);
        let (tx, events) = mpsc::channel(EVENT_CAPACITY);
        let stop = CancellationToken::new();
        let closed = CancellationToken::new();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let stop = stop.clone();
            let closed = closed.clone();
            let in_flight = in_flight.clone();
            async move {
                let _closed = closed.drop_guard();
                run(socket, rx, tx, stop, in_flight).await
            }
        });
        Ok(Self {
            url,
            commands,
            events: Some(events),
            next_id: AtomicU64::new(1),
            in_flight,
            stop,
            closed,
            task: Some(task),
            close_confirmed: None,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn reusable(&self) -> bool {
        !self.stop.is_cancelled()
            && !self.closed.is_cancelled()
            && self.in_flight.load(Ordering::Acquire) == 0
    }

    pub fn take_events(&mut self) -> mpsc::Receiver<CdpEvent> {
        self.events
            .take()
            .expect("event pump owns the receiver once")
    }

    pub async fn next_event(&mut self) -> Option<CdpEvent> {
        self.events.as_mut()?.recv().await
    }

    pub async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        let closed = || CdpError::Closed {
            url: self.url.clone(),
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let exchange = async {
            let (reply, receive) = oneshot::channel();
            self.commands
                .send(Command {
                    id,
                    payload: json!({"id": id, "method": method, "params": params}),
                    reply,
                })
                .await
                .map_err(|_| closed())?;
            receive.await.map_err(|_| closed())?
        };
        tokio::select! {
            biased;
            _ = self.stop.cancelled() => Err(closed()),
            result = tokio::time::timeout(COMMAND_TIMEOUT, exchange) => result.map_err(|_| CdpError::Timeout {
                method: method.to_owned(), timeout_ms: COMMAND_TIMEOUT.as_millis() as u64,
            })?,
        }
    }

    pub async fn close(&mut self) -> Result<(), CdpError> {
        self.stop.cancel();
        if let Some(task) = self.task.as_mut() {
            let confirmed = match tokio::time::timeout(CLOSE_TIMEOUT, &mut *task).await {
                Ok(result) => result.unwrap_or(false),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    false
                }
            };
            self.task = None;
            self.close_confirmed = Some(confirmed);
        }
        if self.close_confirmed == Some(true) {
            Ok(())
        } else {
            Err(CdpError::UnconfirmedClose)
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn run(
    mut socket: Socket,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<CdpEvent>,
    stop: CancellationToken,
    in_flight: Arc<AtomicUsize>,
) -> bool {
    let mut pending = HashMap::new();
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => observe(text.as_str(), &mut pending, &events, &in_flight),
                Some(Ok(Message::Close(frame))) => {
                    if frame.is_some_and(|frame| frame.reason.contains("Target expired")) {
                        for (_, reply) in pending.drain() {
                            let _ = reply.send(Err(CdpError::Rejected { method: "page".to_owned(), message: "Target expired".to_owned() }));
                        }
                        let _ = events.try_send(CdpEvent { method: "Peri.targetExpired".to_owned(), params: Value::Null });
                    }
                    let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.flush()).await;
                    return true;
                }
                Some(Ok(Message::Ping(_))) => {
                    if !send(&mut socket, None, &stop).await { break; }
                }
                Some(Ok(_)) => {},
                None | Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => return true,
                Some(Err(_)) => return false,
            },
            command = commands.recv(), if pending.len() < MAX_PENDING => {
                let Some(command) = command else { break };
                // 先登记 in-flight，再检查 reply 是否关闭：取消后的角色转换不会漏掉已发命令。
                in_flight.fetch_add(1, Ordering::AcqRel);
                if command.reply.is_closed() {
                    in_flight.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                pending.insert(command.id, command.reply);
                if !send(&mut socket, Some(Message::Text(command.payload.to_string().into())), &stop).await { break; }
            }
        }
    }
    drop(commands);
    drop(pending);
    drop(events);
    if socket.send(Message::Close(None)).await.is_err() {
        return false;
    }
    while let Some(message) = socket.next().await {
        match message {
            Ok(Message::Close(_)) | Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                return true
            }
            Err(_) => return false,
            _ => {}
        }
    }
    true
}

async fn send(socket: &mut Socket, message: Option<Message>, stop: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        _ = stop.cancelled() => false,
        result = tokio::time::timeout(COMMAND_TIMEOUT, async {
            match message { Some(message) => socket.send(message).await, None => socket.flush().await }
        }) => matches!(result, Ok(Ok(()))),
    }
}

fn observe(
    text: &str,
    pending: &mut HashMap<u64, Reply>,
    events: &mpsc::Sender<CdpEvent>,
    in_flight: &AtomicUsize,
) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        if let Some(reply) = pending.remove(&id) {
            let result = match value.get("error") {
                Some(error) => Err(CdpError::Protocol {
                    code: error["code"].as_i64().unwrap_or(-32000),
                    message: error["message"]
                        .as_str()
                        .unwrap_or("unknown CDP error")
                        .to_owned(),
                }),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            in_flight.fetch_sub(1, Ordering::AcqRel);
            let _ = reply.send(result);
        }
    } else if let Some(method) = value.get("method").and_then(Value::as_str) {
        let _ = events.try_send(CdpEvent {
            method: method.to_owned(),
            params: value.get("params").cloned().unwrap_or(Value::Null),
        });
    }
}
