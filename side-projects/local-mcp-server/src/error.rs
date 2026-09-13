//! 共享错误类型与错误码冻结边界（WP-001）。
//!
//! 本模块只定义跨包共享的错误契约，不实现任何工具/协议行为。三条规则：
//!
//! 1. **业务错误 ≠ 协议错误**：工具级输入校验、文件系统失败、命令非零退出属于
//!    tool business error，走 `is_error=true` 的 tool result；未知工具、请求形状
//!    损坏、服务器内部失败属于 JSON-RPC error（见 [`ToolError`]）。
//! 2. **错误码取自规范**：MCP 2026-07-28 规定 `-32020..=-32099` 由规范独占，
//!    实现不得发出未定义的值；`-32002` 在本版本必须**不得**发出（被 `-32602` 取代）。
//!    见 [`code`] 模块与 `artifacts/designs/WP-001/protocol-baseline.md` 的条款映射。
//! 3. **错误文本不泄露**：任何进入 wire 的文本都不得包含 bearer token、宿主绝对
//!    路径或挂载外真实路径；使用 [`CapabilityError::public_message`] 取得可外发文本。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 与 MCP 2026-07-28 冻结的错误码。
///
/// 事实源：`https://modelcontextprotocol.io/specification/2026-07-28/basic/index#error-codes`
/// 与 `/schema#headermismatcherror` 等条目；`tests/schema_errors.rs` 会把这些常量
/// 与 rmcp 3.1.4 的 `ErrorCode` 常量逐一比对，防止字符串/数值飘移。
pub mod code {
    /// JSON-RPC 解析失败。
    pub const PARSE_ERROR: i32 = -32700;
    /// JSON-RPC 非法请求。
    pub const INVALID_REQUEST: i32 = -32600;
    /// JSON-RPC 未知方法（Modern 的 `initialize` 未知方法场景）。
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// JSON-RPC 非法参数；本版本的工具未知与请求形状错误都归此码。
    pub const INVALID_PARAMS: i32 = -32602;
    /// JSON-RPC 服务器内部错误。
    pub const INTERNAL_ERROR: i32 = -32603;
    /// HTTP header 与 body 不一致（SEP-2243）。
    pub const HEADER_MISMATCH: i32 = -32020;
    /// 缺少请求所必需的客户端能力（本服务只在 Tasks extension 场景可能触发）。
    pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i32 = -32021;
    /// 不支持的协议版本；`data` 必须带 `supported` 与 `requested`。
    pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;
    /// 2025-11-25 及更早的 resource-not-found 码；本版本实现**必须不得发出**。
    pub const LEGACY_RESOURCE_NOT_FOUND_FORBIDDEN: i32 = -32002;
}

/// 结构化错误载荷，可直接映射到 JSON-RPC `error` 对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// JSON-RPC 错误码。
    pub code: i32,
    /// 面向调用方的人类可读文本（不含 secret / 宿主绝对路径）。
    pub message: String,
    /// 可选的结构化补充信息（如 `supported` / `requested`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ErrorPayload {
    /// 构造不带 `data` 的载荷。
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// 协议级失败：这些失败必须作为 JSON-RPC error 返回，而不是 tool result。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolError {
    /// 工具名不在七个规范工具或已冻结别名表内。
    UnknownTool {
        /// 请求中的原始名称（原样回显，便于模型自纠）。
        name: String,
    },
    /// 请求形状不满足协议 schema（例如 `arguments` 不是对象、`params` 缺失）。
    InvalidRequest {
        /// 面向调用方的说明。
        message: String,
    },
    /// 服务器内部失败，无法服务该请求。
    Internal {
        /// 面向调用方的说明。
        message: String,
    },
    /// 执行面不可用（工具集无法执行该请求）；必须 fail closed，不得回退到
    /// 未授权的执行路径。
    BackendUnavailable {
        /// 面向调用方的说明。
        message: String,
    },
}

impl ToolError {
    /// 映射到规范错误码。
    ///
    /// 未知工具与请求形状错误都使用 `-32602`：规范示例对未知工具使用
    /// `{"code": -32602, "message": "Unknown tool: invalid_tool_name"}`。
    pub fn jsonrpc_code(&self) -> i32 {
        match self {
            Self::UnknownTool { .. } | Self::InvalidRequest { .. } => code::INVALID_PARAMS,
            Self::Internal { .. } | Self::BackendUnavailable { .. } => code::INTERNAL_ERROR,
        }
    }

    /// 面向调用方的消息文本；`UnknownTool` 与规范示例逐字一致。
    pub fn message(&self) -> String {
        match self {
            Self::UnknownTool { name } => format!("Unknown tool: {name}"),
            Self::InvalidRequest { message } | Self::Internal { message } => message.clone(),
            Self::BackendUnavailable { message } => {
                format!("Backend unavailable: {message}")
            }
        }
    }

    /// 转为可序列化的 JSON-RPC 错误载荷。
    pub fn to_payload(&self) -> ErrorPayload {
        ErrorPayload::new(self.jsonrpc_code(), self.message())
    }
}

/// 执行面失败。任何一项都表示**不可继续**：调用方必须 fail closed。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerError {
    /// 执行面无法服务该请求。
    #[error("execution backend unavailable: {reason}")]
    Unavailable {
        /// 不含 secret 的原因说明。
        reason: String,
    },
    /// 信封损坏、版本不匹配、request id 不匹配等协议级异常。
    #[error("execution protocol violation: {reason}")]
    Protocol {
        /// 不含 secret 的原因说明。
        reason: String,
    },
    /// 请求在期限内没有响应。
    #[error("execution request timed out")]
    Timeout,
    /// 执行进程退出（含崩溃）。
    #[error("execution process exited: {code:?}")]
    Exited {
        /// 退出码；被信号杀死时为 `None`。
        code: Option<i32>,
    },
    /// 请求已被取消。
    #[error("execution request cancelled")]
    Cancelled,
}

/// capability 边界错误。
///
/// [`Self::public_message`] 是唯一允许进入 tool result / 日志文本的入口：越界与
/// 符号链接逃逸只回显调用方请求的路径，绝不回显宿主真实根路径或解析结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityError {
    /// 请求路径解析后落在授权根之外。
    OutsideRoot {
        /// 调用方原始请求路径（可回显）。
        requested: String,
        /// 授权根（**不得**回显）。
        root: PathBuf,
    },
    /// 末段或中间段符号链接指向授权根之外。
    SymlinkEscape {
        /// 调用方原始请求路径（可回显）。
        requested: String,
        /// 解析后的真实路径（**不得**回显）。
        resolved: PathBuf,
    },
    /// 参数级校验失败（类型、范围、枚举）。
    InvalidInput {
        /// 面向调用方的说明。
        message: String,
    },
    /// 底层文件系统失败；`message` 必须是已脱敏文本。
    Io {
        /// 已脱敏说明。
        message: String,
    },
}

impl CapabilityError {
    /// 取得可外发的文本：只包含调用方已经知道的路径或已脱敏说明。
    pub fn public_message(&self) -> String {
        match self {
            Self::OutsideRoot { requested, .. } => {
                format!("Path is outside the authorized workspace: {requested}")
            }
            Self::SymlinkEscape { requested, .. } => format!(
                "Path resolves outside the authorized workspace through a symlink: {requested}"
            ),
            Self::InvalidInput { message } | Self::Io { message } => message.clone(),
        }
    }
}

/// broker 配置校验失败（启动期，`main` 必须以 [`exit_code::USAGE`] 退出）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// 字段值非法。
    #[error("invalid configuration for `{field}`: {reason}")]
    InvalidValue {
        /// 字段名（配置键，不是值）。
        field: &'static str,
        /// 不含 secret 的原因说明。
        reason: String,
    },
    /// 缺少必填字段。
    #[error("missing required configuration `{field}`")]
    MissingRequired {
        /// 字段名。
        field: &'static str,
    },
    /// 非回环绑定必须配置 token 来源。
    #[error("non-loopback bind requires a configured token source")]
    TokenRequiredForNonLoopback,
    /// 工作区根不存在或不是目录。
    #[error("workspace root is not an existing directory")]
    WorkspaceNotDirectory,
}

/// 进程退出码表。
///
/// `78`（EX_CONFIG，骨架阶段的"未实现"占位码）**已不存在**：任何启动失败都必须落在
/// 这张表的某个码上，而不是"尚未实现"。随 D-003 去容器化，原 `BACKEND_UNAVAILABLE`
/// 退出码一并删除：启动期失败只有"配置/工作区不成立"（[`exit_code::USAGE`]）与传输/内部失败。
pub mod exit_code {
    /// 正常退出。
    pub const OK: i32 = 0;
    /// 用法/配置错误。
    pub const USAGE: i32 = 2;
    /// 传输层或协议层致命失败。
    pub const TRANSPORT_FAILED: i32 = 4;
    /// 内部错误。
    pub const INTERNAL: i32 = 70;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_message_never_leaks_authorized_root() {
        let err = CapabilityError::OutsideRoot {
            requested: "../../etc/passwd".to_string(),
            root: PathBuf::from("/Users/secret-user/ws-root"),
        };
        let text = err.public_message();
        assert!(
            !text.contains("/Users/secret-user"),
            "public_message 泄露授权根: {text}"
        );
        assert!(text.contains("../../etc/passwd"));
    }

    #[test]
    fn unknown_tool_uses_invalid_params_with_spec_message() {
        let err = ToolError::UnknownTool {
            name: "invalid_tool_name".to_string(),
        };
        assert_eq!(err.jsonrpc_code(), code::INVALID_PARAMS);
        assert_eq!(err.message(), "Unknown tool: invalid_tool_name");
    }
}
