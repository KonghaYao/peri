use std::{collections::HashMap, sync::Arc};

use parking_lot::{Mutex, RwLock};
use serde_json::Value;

use crate::{
    diagnostics::DiagnosticsRegistry, error::LspError, jsonrpc::transport::MessageDispatcher,
};

mod documents;
mod lifecycle;
mod requests;

struct Connection {
    state: ServerState,
    registered: Option<Arc<RegisteredConnection>>,
}

/// Dispatcher 与文档版本缓存属于同一次连接，捕获后不能跨重启重新绑定。
struct RegisteredConnection {
    dispatcher: Arc<MessageDispatcher>,
    open_files: Mutex<HashMap<String, OpenFileInfo>>,
}

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
    connection: Arc<RwLock<Connection>>,
    /// 启动互斥 — 并发 start/try_restart 只有一个执行 do_start；
    /// tokio::sync::Mutex — guard 可以跨 .await 持有
    start_lock: Arc<tokio::sync::Mutex<()>>,
    next_id: Arc<parking_lot::Mutex<i64>>,
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
            connection: Arc::new(RwLock::new(Connection {
                state: ServerState::Stopped,
                registered: None,
            })),
            start_lock: Arc::new(tokio::sync::Mutex::new(())),
            next_id: Arc::new(parking_lot::Mutex::new(0)),
            restart_count: Arc::new(parking_lot::Mutex::new(0)),
            restart_window_start: Arc::new(parking_lot::Mutex::new(None)),
            restart_window: RESTART_WINDOW,
            max_restarts,
            startup_timeout_ms,
            diagnostics,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.connection.read().state == ServerState::Running
    }

    pub fn state(&self) -> ServerState {
        self.connection.read().state.clone()
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
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;

#[cfg(test)]
#[path = "client_lifecycle_test.rs"]
mod lifecycle_tests;

#[cfg(test)]
#[path = "client_document_test.rs"]
mod document_tests;
