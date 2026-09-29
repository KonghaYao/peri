use thiserror::Error;

#[derive(Debug, Error)]
pub enum LspError {
    #[error("LSP 服务器 \"{server}\" 启动失败: {reason}")]
    LaunchFailed { server: String, reason: String },

    #[error("LSP 服务器 \"{server}\" 初始化失败: {reason}")]
    InitFailed { server: String, reason: String },

    #[error("LSP 请求超时 ({method}, {timeout_ms}ms)")]
    RequestTimeout { method: String, timeout_ms: u64 },

    #[error("LSP 请求失败 ({method}): {reason}")]
    RequestFailed { method: String, reason: String },

    #[error("文件内容已被修改，需要重试")]
    ContentModified,

    #[error("服务器 \"{server}\" 已崩溃 (重启次数: {restart_count}/{max_restarts})")]
    ServerCrashed {
        server: String,
        restart_count: u32,
        max_restarts: u32,
    },

    #[error("无可用 LSP 服务器处理文件: {file_path}")]
    NoServerForFile { file_path: String },

    #[error("LSP 服务器 \"{server}\" 未就绪")]
    NotReady { server: String },

    #[error("LSP 服务器连接已断开")]
    TransportClosed,

    #[error("JSON-RPC 错误 (code {code}): {message}")]
    JsonRpcError { code: i64, message: String },

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),
}

impl LspError {
    /// 检查是否为 ContentModified 错误 (LSP error code -32801)
    pub fn is_content_modified(&self) -> bool {
        matches!(
            self,
            LspError::JsonRpcError { code: -32801, .. } | LspError::ContentModified
        )
    }
}
