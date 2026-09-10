//! 消息分发与 pending/后台任务的生命周期；进程管道由父模块构造。

use super::LspTransport;
use crate::{
    error::LspError,
    jsonrpc::{codec, JsonRpcNotification, JsonRpcRequest},
};
use parking_lot::Mutex;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin},
    sync::{mpsc, oneshot},
};

type NotificationHandler = Box<dyn Fn(Value) + Send + Sync>;
type ErrorHandler = Box<dyn Fn(LspError) + Send + Sync>;

/// 分发所需共享状态（从 MessageDispatcher 中提取，供后台 task 使用）
pub struct DispatchState {
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, LspError>>>>,
    notification_handlers: Mutex<HashMap<String, NotificationHandler>>,
    on_error: Mutex<Option<ErrorHandler>>,
    /// stdin 写入端 — dispatch 需向服务器回写响应（如未知请求的 -32601）时使用；
    /// 用 tokio::sync::Mutex 以支持跨 await 持有
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
}

/// 消息分发器：后台读取 stdout，分发到 pending_requests 或 notification_handlers
pub struct MessageDispatcher {
    /// 共享分发状态，供后台 dispatch loop 使用
    dispatch_state: Arc<DispatchState>,
    /// read loop 任务句柄
    read_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stderr_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    dispatch_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    close_lock: tokio::sync::Mutex<()>,
    /// 子进程句柄（与 read task 共享）— close() 先 kill 再 abort read task，避免孤儿进程
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
}

impl MessageDispatcher {
    pub fn new(transport: LspTransport) -> (Self, mpsc::UnboundedReceiver<String>) {
        let stdin = transport.stdin;
        let mut stdout_reader = transport.stdout_reader;
        let mut child = transport.child;
        let stderr = child.stderr.take();

        // 启动 stderr drain 任务
        let stderr_task = stderr.map(|stderr| {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            tracing::debug!(target: "lsp::stderr", "{}", line.trim());
                        }
                        Err(_) => break,
                    }
                }
            })
        });

        // 用 mpsc channel 连接 stdout 读取任务和分发逻辑
        let (tx, rx) = mpsc::unbounded_channel::<String>();

        // 子进程句柄与 read task 共享：EOF 或 close() 时都能 kill
        let child_handle = Arc::new(tokio::sync::Mutex::new(Some(child)));
        let task_child = Arc::clone(&child_handle);

        // 启动 stdout 读取任务（独立 task）
        let read_handle = tokio::spawn(async move {
            loop {
                match codec::decode_message(&mut stdout_reader).await {
                    Ok(Some(msg)) => {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(target: "lsp", "transport EOF");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(target: "lsp", error = %e, "读取消息失败");
                        break;
                    }
                }
            }
            // EOF/读取失败：尝试 kill 子进程（若 close() 已 kill，此处失败无害）
            if let Some(child) = task_child.lock().await.as_mut() {
                let _ = child.kill().await;
            }
        });

        let dispatcher = Self {
            dispatch_state: Arc::new(DispatchState {
                pending: Mutex::new(HashMap::new()),
                notification_handlers: Mutex::new(HashMap::new()),
                on_error: Mutex::new(None),
                stdin: tokio::sync::Mutex::new(Some(stdin)),
            }),
            read_task: Mutex::new(Some(read_handle)),
            stderr_task: Mutex::new(stderr_task),
            dispatch_task: Mutex::new(None),
            close_lock: tokio::sync::Mutex::new(()),
            child: child_handle,
        };

        (dispatcher, rx)
    }

    /// 注册通知处理器
    pub fn on_notification(&self, method: &str, handler: NotificationHandler) {
        self.dispatch_state
            .notification_handlers
            .lock()
            .insert(method.to_string(), handler);
    }

    /// 注册错误回调
    pub fn set_on_error(&self, handler: ErrorHandler) {
        *self.dispatch_state.on_error.lock() = Some(handler);
    }

    /// 注册 pending request（返回 oneshot receiver）
    pub fn register_request(&self, id: i64) -> oneshot::Receiver<Result<Value, LspError>> {
        let (tx, rx) = oneshot::channel();
        self.dispatch_state.pending.lock().insert(id, tx);
        rx
    }

    /// 取消 pending request（请求超时或发送失败时移除注册，避免 oneshot sender 残留）
    ///
    /// 若响应恰好已在途中、条目已被 dispatch 移除，此处为无副作用 no-op。
    pub fn cancel_request(&self, id: i64) {
        self.dispatch_state.pending.lock().remove(&id);
    }

    /// 发送消息到 transport
    pub async fn send_request(&self, request: &JsonRpcRequest) -> Result<(), LspError> {
        let mut guard = self.dispatch_state.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| LspError::JsonRpcError {
            code: -32002,
            message: "transport 已关闭".to_string(),
        })?;
        let body = serde_json::to_string(request)?;
        codec::encode_message(body.as_bytes(), stdin).await
    }

    /// 发送通知到 transport
    pub async fn send_notification(
        &self,
        notification: &JsonRpcNotification,
    ) -> Result<(), LspError> {
        let mut guard = self.dispatch_state.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| LspError::JsonRpcError {
            code: -32002,
            message: "transport 已关闭".to_string(),
        })?;
        let body = serde_json::to_string(notification)?;
        codec::encode_message(body.as_bytes(), stdin).await
    }

    /// 获取共享分发状态的 Arc（供后台 dispatch loop 使用，不持有 tokio::sync::Mutex）
    pub fn dispatch_state(&self) -> Arc<DispatchState> {
        Arc::clone(&self.dispatch_state)
    }

    /// 客户端将分发任务交由同一 owner 关闭；外部仍可直接运行公开分发循环。
    pub(crate) fn start_dispatch_loop(&self, rx: mpsc::UnboundedReceiver<String>) {
        let state = self.dispatch_state();
        *self.dispatch_task.lock() = Some(tokio::spawn(run_dispatch_loop(state, rx)));
    }

    /// 拒绝 pending、关闭管道并回收进程，随后 abort/join 所有自有后台任务。
    pub async fn close(&self) {
        let _closing = self.close_lock.lock().await;
        self.dispatch_state
            .reject_all_pending("LSP transport 已关闭");
        *self.dispatch_state.stdin.lock().await = None;
        if let Some(child) = self.child.lock().await.as_mut() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.kill()).await;
        }
        abort_and_join(&self.read_task).await;
        abort_and_join(&self.stderr_task).await;
        abort_and_join(&self.dispatch_task).await;
    }
}

async fn abort_and_join(slot: &Mutex<Option<tokio::task::JoinHandle<()>>>) {
    let handle = slot.lock().take();
    if let Some(handle) = handle {
        handle.abort();
        let _ = handle.await;
    }
}

impl Drop for MessageDispatcher {
    fn drop(&mut self) {
        self.dispatch_state
            .reject_all_pending("LSP dispatcher 已释放");
        for slot in [&self.read_task, &self.stderr_task, &self.dispatch_task] {
            if let Some(handle) = slot.lock().take() {
                handle.abort();
            }
        }
        if let Ok(mut child) = self.child.try_lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.start_kill();
            }
        }
    }
}

impl DispatchState {
    async fn dispatch(&self, msg: String) {
        let value: Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "lsp", error = %e, "消息解析失败");
                return;
            }
        };

        // 两个方向有独立的 ID 空间，必须先分类 request/notification，再匹配 response。
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            if let Some(id) = value.get("id") {
                if id.is_i64() || id.is_string() || id.is_null() {
                    self.respond_method_not_found(id.clone()).await;
                }
            } else {
                let params = value.get("params").cloned().unwrap_or(Value::Null);
                if let Some(handler) = self.notification_handlers.lock().get(method) {
                    handler(params);
                }
            }
            return;
        }
        // 缺少或同时带有 result/error 的帧不是响应，不能消费仍在等待的请求。
        if value.get("result").is_some() == value.get("error").is_some() {
            return;
        }
        if let Some(id) = value.get("id").and_then(Value::as_i64) {
            let sender = self.pending.lock().remove(&id);
            if let Some(tx) = sender {
                let result = if let Some(error) = value.get("error") {
                    let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32000);
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown error")
                        .to_string();
                    Err(LspError::JsonRpcError { code, message })
                } else {
                    Ok(value["result"].clone())
                };
                let _ = tx.send(result);
            }
        }
    }

    /// 对服务器发起的未知请求回 -32601 MethodNotFound 错误响应（写回 stdin）
    async fn respond_method_not_found(&self, id: Value) {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not found" }
        });
        let body = match serde_json::to_string(&response) {
            Ok(body) => body,
            Err(_) => return,
        };
        if let Some(stdin) = self.stdin.lock().await.as_mut() {
            let _ = codec::encode_message(body.as_bytes(), stdin).await;
        }
    }

    /// 当前 pending 请求数（仅测试断言超时/发送失败后无残留）
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.lock().len()
    }

    /// 拒绝所有待处理请求（transport EOF 或错误时调用）
    fn reject_all_pending(&self, reason: &str) {
        let mut pending = self.pending.lock();
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(LspError::RequestFailed {
                method: "transport".to_string(),
                reason: reason.to_string(),
            }));
        }
    }

    /// 调用 on_error 回调通知上层服务器断开
    fn invoke_on_error(&self, error: LspError) {
        if let Some(handler) = self.on_error.lock().take() {
            handler(error);
        }
    }
}

/// 独立的消息分发循环——接收 Arc<DispatchState> + rx，不持有 tokio::sync::Mutex
pub async fn run_dispatch_loop(state: Arc<DispatchState>, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(msg) = rx.recv().await {
        state.dispatch(msg).await;
    }
    // channel 关闭（stdout EOF 或读取错误），拒绝所有 pending 请求
    tracing::error!(target: "lsp", "LSP transport 断开：stdout EOF，拒绝所有 pending 请求");
    state.reject_all_pending("LSP 服务器已断开连接");
    // 通知上层服务器断开，更新 ServerState
    state.invoke_on_error(LspError::TransportClosed);
}

#[cfg(test)]
#[path = "../transport_test.rs"]
mod tests;
