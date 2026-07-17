//! JSON-RPC 错误码定义与响应构造

/// JSON-RPC 标准错误码
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// acp-hub 自定义错误码
pub const SESSION_NOT_FOUND: i64 = -32000;
pub const SESSION_CRASHED: i64 = -32001;
pub const SPAWN_FAILED: i64 = -32002;
pub const CHILD_TIMEOUT: i64 = -32003;
pub const CHILD_EXITED: i64 = -32004;
