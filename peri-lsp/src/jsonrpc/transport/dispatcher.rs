//! 消息分发与 pending/后台任务的生命周期；进程管道由父模块构造。

mod writer;

use super::LspTransport;
use crate::{
    error::LspError,
    jsonrpc::{codec, JsonRpcNotification, JsonRpcRequest},
};
use parking_lot::Mutex;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Child,
    sync::{mpsc, oneshot},
};

type NotificationHandler = Box<dyn Fn(Value) + Send + Sync>;
type ErrorHandler = Box<dyn Fn(LspError) + Send + Sync>;

pub(crate) type PendingResponse = oneshot::Receiver<Result<Value, LspError>>;

struct PendingEntry {
    token: Arc<()>,
    sender: oneshot::Sender<Result<Value, LspError>>,
}

/// Owns only one registration in its original dispatcher, never the process owner.
#[must_use]
pub(crate) struct PendingRequest {
    state: Weak<DispatchState>,
    id: i64,
    token: Arc<()>,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            let mut admission = state.admission.lock();
            if admission
                .pending
                .get(&self.id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token))
            {
                admission.pending.remove(&self.id);
            }
        }
    }
}

struct Admission {
    closed: bool,
    pending: HashMap<i64, PendingEntry>,
    writer: Option<mpsc::Sender<writer::Frame>>,
}

/// Shared protocol state; no child or task join ownership.
pub struct DispatchState {
    admission: Mutex<Admission>,
    notification_handlers: Mutex<HashMap<String, NotificationHandler>>,
    on_error: Mutex<Option<ErrorHandler>>,
}

/// 队列容量已保留，尚未发送；Drop 在准入前取消且不留下任何帧。
pub(crate) struct NotificationPermit<'a> {
    state: &'a DispatchState,
    permit: mpsc::OwnedPermit<writer::Frame>,
}

/// 已入队帧的写入确认；丢弃 waiter 不会取消 writer 持有的帧。
pub(crate) struct WriteCompletion {
    receiver: oneshot::Receiver<Result<(), LspError>>,
}

impl WriteCompletion {
    pub(crate) async fn wait(self) -> Result<(), LspError> {
        self.receiver.await.map_err(|_| LspError::TransportClosed)?
    }
}

impl NotificationPermit<'_> {
    pub(crate) fn enqueue(
        self,
        notification: &JsonRpcNotification,
    ) -> Result<WriteCompletion, LspError> {
        self.enqueue_body(serde_json::to_string(notification)?)
    }

    fn enqueue_body(self, body: String) -> Result<WriteCompletion, LspError> {
        let admission = self.state.admission.lock();
        if admission.closed {
            return Err(LspError::TransportClosed);
        }
        let (ack, receiver) = oneshot::channel();
        self.permit.send(writer::Frame { body, ack });
        Ok(WriteCompletion { receiver })
    }
}

/// 消息分发器：后台读取 stdout，分发到 pending_requests 或 notification_handlers
pub struct MessageDispatcher {
    /// 共享分发状态，供后台 dispatch loop 使用
    dispatch_state: Arc<DispatchState>,
    writer_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
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

        let (writer_tx, writer_rx) = mpsc::channel(writer::FRAME_QUEUE_CAPACITY);
        let dispatch_state = Arc::new(DispatchState {
            admission: Mutex::new(Admission {
                closed: false,
                pending: HashMap::new(),
                writer: Some(writer_tx),
            }),
            notification_handlers: Mutex::new(HashMap::new()),
            on_error: Mutex::new(None),
        });
        let writer_handle = tokio::spawn(writer::run(
            stdin,
            writer_rx,
            Arc::downgrade(&dispatch_state),
        ));

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
            dispatch_state,
            writer_task: Mutex::new(Some(writer_handle)),
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

    /// Register a request whose entry is removed when its caller future is dropped.
    pub(crate) fn register_owned_request(&self, id: i64) -> (PendingRequest, PendingResponse) {
        let token = Arc::new(());
        let receiver = self.dispatch_state.register_request(id, Arc::clone(&token));
        (
            PendingRequest {
                state: Arc::downgrade(&self.dispatch_state),
                id,
                token,
            },
            receiver,
        )
    }

    /// 注册 pending request（返回 oneshot receiver；调用方负责 cancel_request）。
    pub fn register_request(&self, id: i64) -> oneshot::Receiver<Result<Value, LspError>> {
        self.dispatch_state.register_request(id, Arc::new(()))
    }

    /// 显式取消当前 ID；owned 请求通过其 registration token 自动取消。
    pub fn cancel_request(&self, id: i64) {
        self.dispatch_state.admission.lock().pending.remove(&id);
    }

    /// Enqueue a complete frame and wait for its actual write/flush result.
    pub async fn send_request(&self, request: &JsonRpcRequest) -> Result<(), LspError> {
        self.dispatch_state
            .send_body(serde_json::to_string(request)?)
            .await
    }

    /// 调用者取消不会中断已经入队的帧；成功仍表示实际写入并 flush 完成。
    pub async fn send_notification(
        &self,
        notification: &JsonRpcNotification,
    ) -> Result<(), LspError> {
        self.dispatch_state
            .send_body(serde_json::to_string(notification)?)
            .await
    }

    /// 在文档状态临界区之前等待容量；实际准入必须经 permit 同步完成。
    pub(crate) async fn reserve_notification(&self) -> Result<NotificationPermit<'_>, LspError> {
        self.dispatch_state.reserve_frame().await
    }

    #[cfg(test)]
    pub(crate) fn writer_capacity_for_test(&self) -> Option<usize> {
        self.dispatch_state
            .admission
            .lock()
            .writer
            .as_ref()
            .map(mpsc::Sender::capacity)
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

    /// 同步撤销准入并请求终止；保留 join 槽位供 close 或后续重试回收。
    pub(crate) fn begin_close(&self) {
        self.dispatch_state
            .reject_all_pending("LSP transport 已关闭");
        for slot in [
            &self.writer_task,
            &self.read_task,
            &self.stderr_task,
            &self.dispatch_task,
        ] {
            if let Some(handle) = slot.lock().as_ref() {
                handle.abort();
            }
        }
        if let Ok(mut child) = self.child.try_lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.start_kill();
            }
        }
    }

    /// 拒绝 pending、关闭管道并回收进程，随后 abort/join 所有自有后台任务。
    pub async fn close(&self) {
        let _closing = self.close_lock.lock().await;
        self.begin_close();
        abort_and_join(&self.writer_task).await;
        if let Some(child) = self.child.lock().await.as_mut() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.kill()).await;
        }
        abort_and_join(&self.read_task).await;
        abort_and_join(&self.stderr_task).await;
        abort_and_join(&self.dispatch_task).await;
    }
}

async fn abort_and_join(slot: &Mutex<Option<tokio::task::JoinHandle<()>>>) {
    if let Some(handle) = slot.lock().as_ref() {
        handle.abort();
    }
    // Keep the handle in its owner slot across await: cancelling close must not
    // detach the task and make a later close skip its join.
    std::future::poll_fn(|cx| {
        let mut slot = slot.lock();
        let Some(handle) = slot.as_mut() else {
            return std::task::Poll::Ready(());
        };
        if std::future::Future::poll(std::pin::Pin::new(handle), cx).is_ready() {
            slot.take();
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
}

impl Drop for MessageDispatcher {
    fn drop(&mut self) {
        self.dispatch_state
            .reject_all_pending("LSP dispatcher 已释放");
        for slot in [
            &self.writer_task,
            &self.read_task,
            &self.stderr_task,
            &self.dispatch_task,
        ] {
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
    fn register_request(&self, id: i64, token: Arc<()>) -> PendingResponse {
        let (sender, receiver) = oneshot::channel();
        let mut admission = self.admission.lock();
        if admission.closed {
            let _ = sender.send(Err(LspError::TransportClosed));
        } else {
            admission.pending.insert(id, PendingEntry { token, sender });
        }
        receiver
    }

    async fn reserve_frame(&self) -> Result<NotificationPermit<'_>, LspError> {
        let sender = {
            let admission = self.admission.lock();
            if admission.closed {
                return Err(LspError::TransportClosed);
            }
            admission.writer.clone().ok_or(LspError::TransportClosed)?
        };
        let permit = sender
            .reserve_owned()
            .await
            .map_err(|_| LspError::TransportClosed)?;
        Ok(NotificationPermit {
            state: self,
            permit,
        })
    }

    async fn send_body(&self, body: String) -> Result<(), LspError> {
        self.reserve_frame().await?.enqueue_body(body)?.wait().await
    }

    fn writer_failed(&self) {
        self.reject_all_pending("LSP transport 写入失败");
        self.invoke_on_error(LspError::TransportClosed);
    }

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
            let entry = self.admission.lock().pending.remove(&id);
            if let Some(entry) = entry {
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
                let _ = entry.sender.send(result);
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
        // Writer failure terminates the protocol and reports on_error centrally.
        let _ = self.send_body(body).await;
    }

    /// 当前 pending 请求数（仅测试断言超时/发送失败后无残留）
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.admission.lock().pending.len()
    }

    /// 拒绝所有待处理请求（transport EOF 或错误时调用）
    fn reject_all_pending(&self, reason: &str) {
        let pending = {
            let mut admission = self.admission.lock();
            admission.closed = true;
            admission.writer.take();
            std::mem::take(&mut admission.pending)
        };
        for (_, entry) in pending {
            let _ = entry.sender.send(Err(LspError::RequestFailed {
                method: "transport".to_string(),
                reason: reason.to_string(),
            }));
        }
    }

    /// 调用 on_error 回调通知上层服务器断开
    fn invoke_on_error(&self, error: LspError) {
        let handler = self.on_error.lock().take();
        if let Some(handler) = handler {
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
