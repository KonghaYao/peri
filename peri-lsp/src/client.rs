use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use serde_json::Value;

use crate::{
    diagnostics::DiagnosticsRegistry,
    error::LspError,
    jsonrpc::{transport::MessageDispatcher, JsonRpcNotification, JsonRpcRequest},
    protocol::{
        notifications::{
            did_change_notification, did_open_notification, did_save_notification,
            parse_publish_diagnostics,
        },
        requests::initialize_params,
    },
};

/// LSP 服务器状态
#[derive(Debug, Clone, PartialEq)]
pub enum ServerState {
    Stopped,
    Starting,
    Running,
    Error(String),
}

/// 重启退避窗口：窗口内重启计数不重置，超出 max_restarts 后进入冷却（拒绝重启），
/// 窗口过后计数清零、冷却解除
const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// 启动超时缺省值（毫秒）：`LspServerConfig.startup_timeout` 未配置时使用
pub const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 30_000;

/// 单个 LSP 服务器客户端
pub struct LspClient {
    name: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    initialization_options: Option<Value>,
    state: Arc<RwLock<ServerState>>,
    /// 启动互斥 — 并发 start/try_restart 只有一个执行 do_start；
    /// tokio::sync::Mutex — guard 可以跨 .await 持有
    start_lock: Arc<tokio::sync::Mutex<()>>,
    /// tokio::sync::Mutex — guard 可以跨 .await 持有
    dispatcher: Arc<tokio::sync::Mutex<Option<MessageDispatcher>>>,
    next_id: Arc<parking_lot::Mutex<i64>>,
    open_files: Arc<RwLock<HashMap<String, OpenFileInfo>>>,
    restart_count: Arc<parking_lot::Mutex<u32>>,
    /// 当前重启窗口起点（None = 窗口外，下次重启开启新窗口）
    restart_window_start: Arc<parking_lot::Mutex<Option<std::time::Instant>>>,
    /// 重启计数窗口时长（测试可调短以验证窗口语义）
    restart_window: std::time::Duration,
    max_restarts: u32,
    /// initialize 请求超时（毫秒），来自 `LspServerConfig.startup_timeout`，缺省 30s
    startup_timeout_ms: u64,
    diagnostics: Arc<DiagnosticsRegistry>,
}

#[derive(Debug, Clone)]
struct OpenFileInfo {
    version: i32,
}

enum DidChangeAction {
    Open { language_id: String, version: i32 },
    Change(i32),
}

impl LspClient {
    #[allow(clippy::too_many_arguments)] // 配置透传面：字段逐项注入，与 LspServerConfig 一一对应
    pub fn new(
        name: String,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        initialization_options: Option<Value>,
        max_restarts: u32,
        startup_timeout_ms: u64,
        diagnostics: Arc<DiagnosticsRegistry>,
    ) -> Self {
        Self {
            name,
            command,
            args,
            env,
            initialization_options,
            state: Arc::new(RwLock::new(ServerState::Stopped)),
            start_lock: Arc::new(tokio::sync::Mutex::new(())),
            dispatcher: Arc::new(tokio::sync::Mutex::new(None)),
            next_id: Arc::new(parking_lot::Mutex::new(0)),
            open_files: Arc::new(RwLock::new(HashMap::new())),
            restart_count: Arc::new(parking_lot::Mutex::new(0)),
            restart_window_start: Arc::new(parking_lot::Mutex::new(None)),
            restart_window: RESTART_WINDOW,
            max_restarts,
            startup_timeout_ms,
            diagnostics,
        }
    }

    /// 启动 LSP 服务器并完成 initialize/initialized 握手
    ///
    /// 并发调用安全：同时多个 start 时只有一个执行 do_start（spawn 子进程），
    /// 其余在 start_lock 上等待，完成后检测到 Running 直接复用。
    pub async fn start(&self, root_uri: &str) -> Result<(), LspError> {
        // 快速路径：已运行直接复用（不获取锁，避免无谓排队）
        if *self.state.read() == ServerState::Running {
            return Ok(());
        }

        // 原子化检查-启动：并发 start 只有一个进入 do_start
        let _guard = self.start_lock.lock().await;

        // 二次检查：等待锁期间可能已被其他调用者启动
        if *self.state.read() == ServerState::Running {
            return Ok(());
        }

        let result = self.do_start(root_uri).await;

        {
            let mut state = self.state.write();
            match &result {
                Ok(()) => *state = ServerState::Running, // 已经在 do_start 中设置，这里再次确认
                Err(e) => *state = ServerState::Error(e.to_string()),
            }
        }

        result
    }

    async fn do_start(&self, root_uri: &str) -> Result<(), LspError> {
        let transport =
            crate::jsonrpc::transport::LspTransport::spawn(&self.command, &self.args, &self.env)?;

        let diagnostics = Arc::clone(&self.diagnostics);

        let (dispatcher, rx) = MessageDispatcher::new(transport);

        {
            let diag_clone = Arc::clone(&diagnostics);
            dispatcher.on_notification(
                "textDocument/publishDiagnostics",
                Box::new(move |params: Value| {
                    if let Some(publish_params) = parse_publish_diagnostics(&params) {
                        diag_clone.handle_publish_diagnostics(&publish_params);
                    }
                }),
            );
        }

        {
            let state = Arc::clone(&self.state);
            let name = self.name.clone();
            dispatcher.set_on_error(Box::new(move |error: LspError| {
                tracing::warn!(target: "lsp", server = %name, error = %error, "LSP 服务器错误");
                *state.write() = ServerState::Error(error.to_string());
            }));
        }

        *self.dispatcher.lock().await = Some(dispatcher);

        // 提取共享分发状态（Arc clone），不持有 tokio::sync::Mutex
        let dispatch_state = {
            let guard = self.dispatcher.lock().await;
            guard.as_ref().unwrap().dispatch_state()
        };

        // 立即设置状态为 Running，这样 initialize 请求可以通过状态检查
        *self.state.write() = ServerState::Running;

        // 启动消息分发循环（后台 task，消费 stdout 消息）
        // 使用 Arc<DispatchState> 而非持有 tokio::sync::Mutex guard，避免死锁
        tokio::spawn(async move {
            crate::jsonrpc::transport::run_dispatch_loop(dispatch_state, rx).await;
        });

        // root_uri 已经是 "file:///path" 格式，直接使用
        let workspace_uri: lsp_types::Uri = root_uri
            .parse()
            .unwrap_or_else(|_| "file:///tmp".parse().unwrap());
        let workspace_folders = vec![lsp_types::WorkspaceFolder {
            uri: workspace_uri,
            name: "workspace".to_string(),
        }];

        let init_params = initialize_params(
            root_uri.to_string(),
            workspace_folders,
            self.initialization_options.clone(),
        );

        let result = self
            .request("initialize", Some(init_params), self.startup_timeout_ms)
            .await;
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                // 启动失败：清理已 spawn 的子进程与 read task，避免孤儿进程
                // （与 close() 语义一致：先 kill 子进程再 abort read task；
                // 否则子进程永远等不到 shutdown/exit，stdin 未关不会 EOF）
                if let Some(d) = self.dispatcher.lock().await.as_ref() {
                    d.close().await;
                }
                *self.dispatcher.lock().await = None;
                return Err(e);
            }
        };

        let _server_capabilities = result.get("capabilities").cloned();
        tracing::info!(
            target: "lsp",
            server = %self.name,
            "LSP 服务器初始化成功"
        );

        if let Err(e) = self
            .notify("initialized", Some(Value::Object(Default::default())))
            .await
        {
            // 与 initialize 失败路径一致：清理子进程与 read task
            if let Some(d) = self.dispatcher.lock().await.as_ref() {
                d.close().await;
            }
            *self.dispatcher.lock().await = None;
            return Err(e);
        }

        Ok(())
    }

    fn next_request_id(&self) -> i64 {
        let mut id = self.next_id.lock();
        *id += 1;
        *id
    }

    /// 发送请求并等待响应（带超时）
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_ms: u64,
    ) -> Result<Value, LspError> {
        let state = self.state.read().clone();
        if state != ServerState::Running {
            return Err(LspError::NotReady {
                server: self.name.clone(),
            });
        }

        let id = self.next_request_id();
        let request = JsonRpcRequest::new(id, method, params);

        let receiver = {
            let guard = self.dispatcher.lock().await;
            match guard.as_ref() {
                Some(d) => d.register_request(id),
                None => {
                    return Err(LspError::NotReady {
                        server: self.name.clone(),
                    })
                }
            }
        };

        {
            let mut guard = self.dispatcher.lock().await;
            match guard.as_mut() {
                Some(d) => {
                    if let Err(e) = d.send_request(&request).await {
                        tracing::error!(
                            target: "lsp",
                            server = %self.name,
                            method,
                            error = %e,
                            "LSP 请求发送失败（服务器可能已崩溃）"
                        );
                        // 发送失败：请求未发出，pending 注册同样成为孤儿，一并移除
                        d.cancel_request(id);
                        return Err(e);
                    }
                }
                None => {
                    return Err(LspError::NotReady {
                        server: self.name.clone(),
                    })
                }
            }
        }

        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(LspError::RequestFailed {
                method: method.to_string(),
                reason: "请求被取消".to_string(),
            }),
            Err(_) => {
                // 超时：从 pending 移除，避免 oneshot sender 残留
                // （此前仅在 transport EOF 时由 reject_all_pending 整体清理）
                if let Some(d) = self.dispatcher.lock().await.as_ref() {
                    d.cancel_request(id);
                }
                Err(LspError::RequestTimeout {
                    method: method.to_string(),
                    timeout_ms,
                })
            }
        }
    }

    /// 发送通知
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), LspError> {
        let notification = JsonRpcNotification::new(method, params);
        let mut guard = self.dispatcher.lock().await;
        match guard.as_mut() {
            Some(d) => d.send_notification(&notification).await,
            None => Err(LspError::NotReady {
                server: self.name.clone(),
            }),
        }
    }

    /// 文件同步: didOpen
    pub async fn did_open(&self, uri: &str, language_id: &str, text: &str) -> Result<(), LspError> {
        let version = {
            let mut open = self.open_files.write();
            if open.contains_key(uri) {
                return Ok(());
            }
            let v = open.len() as i32 + 1;
            open.insert(uri.to_string(), OpenFileInfo { version: v });
            v
        };

        let notif = did_open_notification(uri, language_id, version, text);
        let mut guard = self.dispatcher.lock().await;
        match guard.as_mut() {
            Some(d) => d.send_notification(&notif).await,
            None => Err(LspError::NotReady {
                server: self.name.clone(),
            }),
        }
    }

    /// 文件同步: didChange
    pub async fn did_change(&self, uri: &str, text: &str) -> Result<(), LspError> {
        // 所有版本号操作同步完成（不跨 await），避免 parking_lot guard 的 Send 问题
        let action = {
            let mut open = self.open_files.write();
            if let Some(info) = open.get_mut(uri) {
                info.version += 1;
                DidChangeAction::Change(info.version)
            } else {
                let v = open.len() as i32 + 1;
                let language_id = Self::infer_language_id(uri);
                open.insert(uri.to_string(), OpenFileInfo { version: v });
                DidChangeAction::Open {
                    language_id,
                    version: v,
                }
            }
        };

        match action {
            DidChangeAction::Open {
                language_id,
                version,
            } => {
                let notif = did_open_notification(uri, &language_id, version, text);
                let mut guard = self.dispatcher.lock().await;
                match guard.as_mut() {
                    Some(d) => d.send_notification(&notif).await,
                    None => Err(LspError::NotReady {
                        server: self.name.clone(),
                    }),
                }
            }
            DidChangeAction::Change(version) => {
                let notif = did_change_notification(uri, version, text);
                let mut guard = self.dispatcher.lock().await;
                match guard.as_mut() {
                    Some(d) => d.send_notification(&notif).await,
                    None => Err(LspError::NotReady {
                        server: self.name.clone(),
                    }),
                }
            }
        }
    }

    /// 文件同步: didSave
    pub async fn did_save(&self, uri: &str) -> Result<(), LspError> {
        let notif = did_save_notification(uri, None);
        let mut guard = self.dispatcher.lock().await;
        match guard.as_mut() {
            Some(d) => d.send_notification(&notif).await,
            None => Err(LspError::NotReady {
                server: self.name.clone(),
            }),
        }
    }

    pub fn is_ready(&self) -> bool {
        *self.state.read() == ServerState::Running
    }

    pub fn state(&self) -> ServerState {
        self.state.read().clone()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn infer_language_id(uri: &str) -> String {
        let ext = std::path::Path::new(uri)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "rs" => "rust".to_string(),
            "ts" => "typescript".to_string(),
            "tsx" => "typescriptreact".to_string(),
            "js" => "javascript".to_string(),
            "jsx" => "javascriptreact".to_string(),
            "py" => "python".to_string(),
            "go" => "go".to_string(),
            "java" => "java".to_string(),
            "c" => "c".to_string(),
            "cpp" | "cc" | "cxx" => "cpp".to_string(),
            "h" | "hpp" => "c".to_string(),
            "rb" => "ruby".to_string(),
            "swift" => "swift".to_string(),
            "kt" | "kts" => "kotlin".to_string(),
            other => other.to_string(),
        }
    }

    pub async fn shutdown(&self) {
        let _ = self.request("shutdown", Some(Value::Null), 5_000).await;
        let _ = self.notify("exit", None).await;

        let guard = self.dispatcher.lock().await;
        if let Some(d) = guard.as_ref() {
            d.close().await;
        }

        *self.state.write() = ServerState::Stopped;
    }

    /// 检查重启次数限制并递增计数（同步操作，确保 parking_lot guard 不跨 await）。
    ///
    /// 时间窗退避：计数只在窗口内累计，窗口（默认 60s）过后清零重新累计；
    /// 窗口内计数达到 max_restarts 后返回 ServerCrashed 并进入冷却，
    /// 冷却期内拒绝重启，直到窗口结束。
    fn check_and_increment_restart(&self) -> Result<(), LspError> {
        let mut count = self.restart_count.lock();
        let mut window_start = self.restart_window_start.lock();
        let now = std::time::Instant::now();

        // 窗口已过期：清零计数并开启新窗口；首次重启同样开启窗口
        if let Some(start) = *window_start {
            if now.duration_since(start) >= self.restart_window {
                *count = 0;
                *window_start = Some(now);
            }
        } else {
            *window_start = Some(now);
        }

        if *count >= self.max_restarts {
            return Err(LspError::ServerCrashed {
                server: self.name.clone(),
                restart_count: *count,
                max_restarts: self.max_restarts,
            });
        }
        *count += 1;
        Ok(())
    }

    pub async fn try_restart(&self, root_uri: &str) -> Result<(), LspError> {
        self.check_and_increment_restart()?;

        // 与 start 互斥：避免重启与并发启动时双重 spawn
        let _guard = self.start_lock.lock().await;

        {
            let guard = self.dispatcher.lock().await;
            if let Some(d) = guard.as_ref() {
                d.close().await;
            }
        }
        *self.dispatcher.lock().await = None;
        self.open_files.write().clear();
        // 重启后清除旧诊断：服务器状态已重置，残留诊断属过期信息
        self.diagnostics.clear_all();

        match self.do_start(root_uri).await {
            Ok(()) => {
                *self.state.write() = ServerState::Running;
                Ok(())
            }
            Err(e) => {
                *self.state.write() = ServerState::Error(e.to_string());
                Err(e)
            }
        }
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
