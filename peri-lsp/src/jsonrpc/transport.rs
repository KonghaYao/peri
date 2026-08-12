use std::{collections::HashMap, process::Stdio, sync::Arc};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::{mpsc, oneshot},
};

use crate::{
    error::LspError,
    jsonrpc::{codec, JsonRpcNotification, JsonRpcRequest},
};

type NotificationHandler = Box<dyn Fn(Value) + Send + Sync>;
type ErrorHandler = Box<dyn Fn(LspError) + Send + Sync>;

/// LSP 传输层：管理子进程的 stdin/stdout/stderr 管道
pub struct LspTransport {
    child: Child,
    stdin: ChildStdin,
    stdout_reader: BufReader<ChildStdout>,
}

impl LspTransport {
    /// 启动 LSP 服务器子进程
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, LspError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| LspError::LaunchFailed {
            server: command.to_string(),
            reason: e.to_string(),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| LspError::LaunchFailed {
            server: command.to_string(),
            reason: "无法获取 stdin".to_string(),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| LspError::LaunchFailed {
            server: command.to_string(),
            reason: "无法获取 stdout".to_string(),
        })?;

        // 启动后立即检查进程是否存活（捕获参数错误等立即退出的情况）
        // 对参数无效等场景，进程退出极快，try_wait 通常能立即捕获
        if let Some(status) = child.try_wait().ok().flatten() {
            let code = status.code().unwrap_or(-1);
            let reason = format!("进程立即退出 (exit code: {code})，请检查命令和参数是否正确");
            return Err(LspError::LaunchFailed {
                server: command.to_string(),
                reason,
            });
        }

        Ok(Self {
            child,
            stdin,
            stdout_reader: BufReader::new(stdout),
        })
    }

    /// 发送 JSON-RPC 请求
    pub async fn send_request(&mut self, request: &JsonRpcRequest) -> Result<(), LspError> {
        let body = serde_json::to_string(request)?;
        codec::encode_message(body.as_bytes(), &mut self.stdin).await
    }

    /// 发送 JSON-RPC 通知
    pub async fn send_notification(
        &mut self,
        notification: &JsonRpcNotification,
    ) -> Result<(), LspError> {
        let body = serde_json::to_string(notification)?;
        codec::encode_message(body.as_bytes(), &mut self.stdin).await
    }

    /// 读取单条 JSON-RPC 消息
    pub async fn read_message(&mut self) -> Result<Option<String>, LspError> {
        codec::decode_message(&mut self.stdout_reader).await
    }

    /// 检查子进程是否存活
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// 获取子进程 ID
    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    /// 终止子进程
    pub async fn kill(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

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
        if let Some(stderr) = stderr {
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
            });
        }

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

    /// 关闭 transport：先终止子进程（短等待），再 abort read task
    ///
    /// 顺序不能反：直接 abort read task 会跳过其中的 `child.kill()`，
    /// 子进程失去 stdout 消费者后继续存活，成为孤儿进程。
    pub async fn close(&self) {
        *self.dispatch_state.stdin.lock().await = None;
        if let Some(child) = self.child.lock().await.as_mut() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.kill()).await;
        }
        if let Some(handle) = self.read_task.lock().take() {
            handle.abort();
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

        if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
            let sender = self.pending.lock().remove(&id);
            match sender {
                Some(tx) => {
                    let result = if let Some(error) = value.get("error") {
                        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-32000);
                        let message = error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("Unknown error")
                            .to_string();
                        Err(LspError::JsonRpcError { code, message })
                    } else {
                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = tx.send(result);
                }
                None => {
                    // 服务器发起的请求（客户端未注册该 id）。按 JSON-RPC/LSP 规范，
                    // 带 id 的请求必须回响应；未知方法回 -32601 MethodNotFound，
                    // 否则服务器会同步等待响应，后续 textDocument 请求排队直至超时。
                    if value.get("method").and_then(|m| m.as_str()).is_some() {
                        self.respond_method_not_found(id).await;
                    }
                }
            }
        } else if let Some(method) = value.get("method").and_then(|m| m.as_str()) {
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let handlers = self.notification_handlers.lock();
            if let Some(handler) = handlers.get(method) {
                handler(params);
            }
        }
    }

    /// 对服务器发起的未知请求回 -32601 MethodNotFound 错误响应（写回 stdin）
    async fn respond_method_not_found(&self, id: i64) {
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
#[path = "transport_test.rs"]
mod tests;
