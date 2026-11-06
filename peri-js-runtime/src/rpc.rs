use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio::task::JoinHandle;

use crate::{JsRuntimeError, ResourceKind, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub enum ParsedMessage {
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    },
    Request {
        id: Option<u64>,
        method: String,
        params: Option<Value>,
    },
}

pub enum IncomingMessage {
    ProtocolError(String),
    ResourceLimit {
        resource: ResourceKind,
        limit: usize,
        observed: usize,
    },
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    },
    Request {
        id: Option<u64>,
        method: String,
        params: Option<Value>,
    },
}

pub struct RpcChannel {
    stdin: Mutex<ChildStdin>,
    pending_requests: Arc<DashMap<u64, oneshot::Sender<std::result::Result<Value, JsonRpcError>>>>,
    next_id: AtomicU64,
    max_frame_bytes: usize,
}

impl RpcChannel {
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn new(stdin: ChildStdin, max_frame_bytes: usize) -> Self {
        Self {
            stdin: Mutex::new(stdin),
            pending_requests: Arc::new(DashMap::new()),
            next_id: AtomicU64::new(1),
            max_frame_bytes,
        }
    }

    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let mut message = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method});
        if !params.is_null() {
            message["params"] = params;
        }
        self.pending_requests.insert(id, tx);
        if let Err(error) = self.write_line(&message).await {
            self.pending_requests.remove(&id);
            return Err(error);
        }
        match rx.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(JsRuntimeError::RpcResponse(error)),
            Err(_) => Err(JsRuntimeError::Rpc("pending request cancelled".to_owned())),
        }
    }

    pub async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        let mut message = serde_json::json!({"jsonrpc": "2.0", "method": method});
        if !params.is_null() {
            message["params"] = params;
        }
        self.write_line(&message).await
    }

    pub async fn send_response(&self, id: u64, result: Value) -> Result<()> {
        self.write_line(&serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await
    }

    pub async fn send_error(
        &self,
        id: u64,
        code: i32,
        message: &str,
        data: Option<Value>,
    ) -> Result<()> {
        let mut error = serde_json::json!({"code": code, "message": message});
        if let Some(data) = data {
            error["data"] = data;
        }
        self.write_line(&serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error}))
            .await
    }

    async fn write_line(&self, value: &Value) -> Result<()> {
        let line = serde_json::to_vec(value)?;
        if line.len() > self.max_frame_bytes {
            return Err(JsRuntimeError::ResourceLimit {
                resource: ResourceKind::FrameBytes,
                limit: self.max_frame_bytes,
                observed: line.len(),
            });
        }
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    pub fn drain_pending(&self, reason: &str) {
        let keys: Vec<u64> = self
            .pending_requests
            .iter()
            .map(|entry| *entry.key())
            .collect();
        for key in keys {
            if let Some((_, sender)) = self.pending_requests.remove(&key) {
                let _ = sender.send(Err(JsonRpcError {
                    code: -32000,
                    message: reason.to_owned(),
                    data: None,
                }));
            }
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn handle_incoming(&self, raw: &str) -> Option<IncomingMessage> {
        let parsed = match parse_message(raw) {
            Ok(parsed) => parsed,
            Err(_) => {
                self.drain_pending("JavaScript RPC protocol error");
                return Some(IncomingMessage::ProtocolError(
                    "JavaScript RPC protocol error".into(),
                ));
            }
        };
        match parsed {
            ParsedMessage::Response { id, result, error } => {
                if let Some((_, sender)) = self.pending_requests.remove(&id) {
                    let response = match error.as_ref() {
                        Some(error) => Err(error.clone()),
                        None => Ok(result.unwrap_or(Value::Null)),
                    };
                    let _ = sender.send(response);
                    None
                } else {
                    Some(IncomingMessage::Response { id, result, error })
                }
            }
            ParsedMessage::Request { id, method, params } => {
                Some(IncomingMessage::Request { id, method, params })
            }
        }
    }
}

pub fn parse_message(raw: &str) -> Result<ParsedMessage> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| JsRuntimeError::Rpc(format!("malformed JSON-RPC frame: {error}")))?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(JsRuntimeError::Rpc("malformed JSON-RPC frame".into()));
    }
    let id = value.get("id").and_then(Value::as_u64);
    if value.get("result").is_some() || value.get("error").is_some() {
        let id = id.ok_or_else(|| JsRuntimeError::Rpc("malformed JSON-RPC response".into()))?;
        let error = value
            .get("error")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                JsRuntimeError::Rpc(format!("malformed JSON-RPC error response: {error}"))
            })?;
        return Ok(ParsedMessage::Response {
            id,
            result: value.get("result").cloned(),
            error,
        });
    }
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| JsRuntimeError::Rpc("malformed JSON-RPC request".into()))?;
    Ok(ParsedMessage::Request {
        id,
        method: method.to_owned(),
        params: value.get("params").cloned(),
    })
}

/// 等待宿主发出终止信号（发送端被丢弃也视同终止）。
async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow_and_update() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

/// 入站投递与终止信号竞速。
///
/// 队列满时 `send` 会等待消费者；终止路径（启动失败/超时/kill）可能根本没有
/// 消费者，必须让 reader 能退出，否则 `join_readers` 永久挂起。
async fn deliver(
    sender: &mpsc::Sender<IncomingMessage>,
    shutdown: &mut watch::Receiver<bool>,
    message: IncomingMessage,
) -> bool {
    tokio::select! {
        biased;
        _ = wait_for_shutdown(shutdown) => false,
        result = sender.send(message) => result.is_ok(),
    }
}

/// 把 stdout 的 NDJSON 帧解码为入站消息；终止信号置位时停止投递并退出。
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn spawn_stdout_reader(
    stdout: ChildStdout,
    channel: Arc<RpcChannel>,
    sender: mpsc::Sender<IncomingMessage>,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut frame = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            frame.clear();
            let read = loop {
                let available = match reader.fill_buf().await {
                    Ok(available) => available,
                    Err(_) => {
                        let _ = deliver(
                            &sender,
                            &mut shutdown,
                            IncomingMessage::ProtocolError("JavaScript RPC protocol error".into()),
                        )
                        .await;
                        channel.drain_pending("JavaScript RPC protocol error");
                        return;
                    }
                };
                if available.is_empty() {
                    break 0;
                }
                let take = available
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(available.len(), |index| index + 1)
                    .min(chunk.len())
                    .min(channel.max_frame_bytes.saturating_add(1) - frame.len());
                let copied = reader.read(&mut chunk[..take]).await.unwrap_or(0);
                frame.extend_from_slice(&chunk[..copied]);
                if frame.len() > channel.max_frame_bytes {
                    channel.drain_pending("JavaScript RPC frame exceeded limit");
                    let _ = deliver(
                        &sender,
                        &mut shutdown,
                        IncomingMessage::ResourceLimit {
                            resource: ResourceKind::FrameBytes,
                            limit: channel.max_frame_bytes,
                            observed: frame.len(),
                        },
                    )
                    .await;
                    return;
                }
                if copied == 0 || frame.last() == Some(&b'\n') {
                    break frame.len();
                }
            };
            if read == 0 {
                break;
            }
            if frame.last() == Some(&b'\n') {
                frame.pop();
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
            }
            if frame.len() > channel.max_frame_bytes {
                channel.drain_pending("JavaScript RPC frame exceeded limit");
                let _ = deliver(
                    &sender,
                    &mut shutdown,
                    IncomingMessage::ResourceLimit {
                        resource: ResourceKind::FrameBytes,
                        limit: channel.max_frame_bytes,
                        observed: frame.len(),
                    },
                )
                .await;
                break;
            }
            let line = match std::str::from_utf8(&frame) {
                Ok(line) => line,
                Err(_) => {
                    let _ = deliver(
                        &sender,
                        &mut shutdown,
                        IncomingMessage::ProtocolError("JavaScript RPC protocol error".into()),
                    )
                    .await;
                    break;
                }
            };
            if let Some(message) = channel.handle_incoming(line) {
                if !deliver(&sender, &mut shutdown, message).await {
                    break;
                }
            }
        }
        channel.drain_pending("JavaScript process exited");
    })
}

#[cfg(test)]
#[path = "rpc_test.rs"]
mod tests;
