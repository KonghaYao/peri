use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use serde_json::Value;

use crate::{
    diagnostics::DiagnosticsRegistry,
    error::LspError,
    jsonrpc::transport::MessageDispatcher,
    protocol::notifications::{
        did_change_notification, did_open_notification, did_save_notification,
    },
};

mod lifecycle;
mod requests;

struct Connection {
    state: ServerState,
    dispatcher: Option<Arc<MessageDispatcher>>,
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
            connection: Arc::new(RwLock::new(Connection {
                state: ServerState::Stopped,
                dispatcher: None,
            })),
            start_lock: Arc::new(tokio::sync::Mutex::new(())),
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
        self.ready_dispatcher()?.send_notification(&notif).await
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
                self.ready_dispatcher()?.send_notification(&notif).await
            }
            DidChangeAction::Change(version) => {
                let notif = did_change_notification(uri, version, text);
                self.ready_dispatcher()?.send_notification(&notif).await
            }
        }
    }

    /// 文件同步: didSave
    pub async fn did_save(&self, uri: &str) -> Result<(), LspError> {
        let notif = did_save_notification(uri, None);
        self.ready_dispatcher()?.send_notification(&notif).await
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
