//! 共享 wire 契约（WP-001 冻结；D-003 去容器化后只保留 MCP 面 DTO）。
//!
//! 本模块是整个交付的**唯一 DTO 冻结点**：协议版本、`_meta` 键、工具注册面、
//! MCP 面请求/结果结构、任务状态 DTO 与执行缝 trait。容器期的宿主↔worker 信封
//! （`WorkerRequest`/`WorkerResponse`/`WorkerOperation`/`WorkerClient`）已随单进程
//! 形态删除——生产路径见 [`crate::runtime`]，那里没有帧、没有配对、没有通道监督。
//!
//! 后续包只能消费本模块的类型，不得重新定义同名类型。
//!
//! 规范性来源（逐条核对于 `artifacts/designs/WP-001/protocol-baseline.md`）：
//! - `https://modelcontextprotocol.io/specification/2026-07-28/basic/index#meta`
//! - `https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning`
//! - `https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http`
//! - `https://modelcontextprotocol.io/specification/2026-07-28/server/tools`

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::ToolError;

// ───────────────────────────── 协议版本与元数据 ─────────────────────────────

/// Modern 协议版本：每次请求自带版本/身份/能力，无握手、无会话。
pub const MCP_MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

/// Legacy 协议版本：保留 `initialize`/`initialized` 握手与会话语义。
pub const MCP_LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

/// 本服务声明支持的协议版本（`server/discover` 的 `supportedVersions`）。
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] =
    [MCP_MODERN_PROTOCOL_VERSION, MCP_LEGACY_PROTOCOL_VERSION];

/// `_meta` 键：本请求使用的协议版本（modern 每个请求必填）。
pub const META_KEY_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` 键：客户端身份（可选，仅用于显示/日志，**不得**用于授权）。
pub const META_KEY_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
/// `_meta` 键：客户端能力（modern 每个请求必填）。
pub const META_KEY_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// `_meta` 键：服务端身份（结果中 SHOULD 携带）。
pub const META_KEY_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
/// `_meta` 键：把通知关联回 `subscriptions/listen` 请求。
pub const META_KEY_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

/// Modern 请求在 `_meta` 中**必须**携带的键；缺失即 `-32602`（HTTP 200 → 400）。
pub const REQUIRED_REQUEST_META_KEYS: [&str; 2] =
    [META_KEY_PROTOCOL_VERSION, META_KEY_CLIENT_CAPABILITIES];

/// 结果判别字段 `resultType` 的成功值。
pub const RESULT_TYPE_COMPLETE: &str = "complete";
/// 结果判别字段 `resultType` 的 MRTR 值（本服务不主动使用，仅登记）。
pub const RESULT_TYPE_INPUT_REQUIRED: &str = "input_required";

/// Streamable HTTP：协议版本头（每个 modern 请求必带）。
pub const HEADER_MCP_PROTOCOL_VERSION: &str = "MCP-Protocol-Version";
/// Streamable HTTP：方法名头（所有请求必带，SEP-2243）。
pub const HEADER_MCP_METHOD: &str = "Mcp-Method";
/// Streamable HTTP：工具/资源名头（`tools/call`、`resources/read`、`prompts/get` 必带）。
pub const HEADER_MCP_NAME: &str = "Mcp-Name";
/// Streamable HTTP：从工具参数映射的自定义头前缀。
pub const HEADER_MCP_PARAM_PREFIX: &str = "Mcp-Param-";

/// HTTP body 上限默认值：4 MiB。
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024;

// ───────────────────────────────── 工具注册面 ────────────────────────────────

/// 对外暴露的七个工具，顺序即 `tools/list` 的确定性顺序。
pub const TOOL_NAMES: [&str; 7] = [
    "Read",
    "Write",
    "Edit",
    "Glob",
    "Grep",
    "folder_operations",
    "Bash",
];

/// 名称别名 → 规范名。
///
/// 别名**不是**额外的 `tools/list` 条目：`tools/list` 恰好返回 [`TOOL_NAMES`] 的
/// 七个名字，别名只在 `tools/call` 的名称解析中生效（源事实：`read.rs` 的
/// `aliases()=["reading"]`、`terminal.rs` 的 `aliases()=["Shell"]`）。
pub const TOOL_ALIASES: [(&str, &str); 2] = [("reading", "Read"), ("Shell", "Bash")];

/// 把请求中的工具名解析为规范名；未知名称返回 `None`。
pub fn resolve_tool_name(name: &str) -> Option<&'static str> {
    if let Some(canonical) = TOOL_NAMES.iter().find(|candidate| **candidate == name) {
        return Some(canonical);
    }
    TOOL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, canonical)| *canonical)
}

/// 断言工具名集合不含重复且别名不占用规范名（供实现与测试共同使用）。
pub fn tool_name_set() -> BTreeSet<&'static str> {
    TOOL_NAMES.iter().copied().collect()
}

// ──────────────────────────────── 请求上下文 DTO ─────────────────────────────

/// JSON-RPC 请求 id（字符串或数字在入口处统一转成字符串形式）。
pub type RequestId = String;
/// 授权主体：**只**由认证层或 stdio 连接实例产生，绝不来自 `clientInfo`。
pub type PrincipalId = String;
/// 连接实例：每条可信连接唯一、不可猜；状态句柄绑定 `(principal, instance)`。
pub type ClientInstanceId = String;
/// 任务 id：`shell-<UUIDv7>`。
pub type TaskId = String;

/// 单次工具调用的上下文。
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// 请求 id。
    pub request_id: RequestId,
    /// 可信主体。
    pub principal: PrincipalId,
    /// 可信连接实例。
    pub client_instance: ClientInstanceId,
    /// 取消信号；请求被 `notifications/cancelled` 或连接关闭时触发。
    pub cancellation: CancellationToken,
}

impl RequestContext {
    /// 以独占主体/实例构造上下文（实现者在认证层之外不得自行构造 principal）。
    pub fn new(
        request_id: impl Into<RequestId>,
        principal: impl Into<PrincipalId>,
        client_instance: impl Into<ClientInstanceId>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            principal: principal.into(),
            client_instance: client_instance.into(),
            cancellation: CancellationToken::new(),
        }
    }
}

/// 工具调用请求（已通过名称解析与协议校验）。
#[derive(Debug, Clone)]
pub struct ToolRequest {
    /// 规范工具名（解析别名之后）。
    pub name: &'static str,
    /// 原始参数对象；字段校验由各工具实现负责。
    pub arguments: serde_json::Value,
    /// 调用上下文。
    pub context: RequestContext,
}

/// 工具调用结果。
///
/// `structured` 的键遵循 WP-000 冻结的 snake_case 形状，见 [`StructuredOutput`]。
/// 业务失败必须构造为 [`ToolResponse::tool_error`]，而不是返回 [`ToolError`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResponse {
    /// 人类可读文本，保留源实现文案。
    pub text: String,
    /// 结构化结果，至少包含 [`StructuredOutput`] 的字段。
    pub structured: serde_json::Value,
    /// 是否为工具业务错误（映射到 `CallToolResult.isError`）。
    pub is_error: bool,
    /// 结果级 `_meta`（例如 `io.modelcontextprotocol/serverInfo`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ToolResponse {
    /// 成功结果。
    pub fn ok(text: impl Into<String>, structured: StructuredOutput) -> Self {
        Self {
            text: text.into(),
            structured: serde_json::to_value(structured).unwrap_or(serde_json::Value::Null),
            is_error: false,
            meta: None,
        }
    }

    /// 业务错误结果（`isError: true`），文本必须已脱敏。
    pub fn tool_error(text: impl Into<String>, structured: StructuredOutput) -> Self {
        Self {
            text: text.into(),
            structured: serde_json::to_value(structured).unwrap_or(serde_json::Value::Null),
            is_error: true,
            meta: None,
        }
    }

    /// 从任意结构化 JSON 构造成功结果（工具自定义形状时使用）。
    pub fn ok_with_json(text: impl Into<String>, structured: serde_json::Value) -> Self {
        Self {
            text: text.into(),
            structured,
            is_error: false,
            meta: None,
        }
    }

    /// 从任意结构化 JSON 构造业务错误结果。
    pub fn tool_error_with_json(text: impl Into<String>, structured: serde_json::Value) -> Self {
        Self {
            text: text.into(),
            structured,
            is_error: true,
            meta: None,
        }
    }
}

/// [`ToolResponse::structured`] 的冻结基线字段。
///
/// 工具可以在 `extra` 中追加自有字段，但**不得**删除或改名基线字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredOutput {
    /// 规范工具名。
    pub tool: String,
    /// 与 [`ToolResponse::is_error`] 一致的成功标志，便于无文本消费者判断。
    pub ok: bool,
    /// 输出是否被截断（含行/字节/条目任一上限）。
    pub truncated: bool,
    /// 落盘路径（只有真正落盘时存在）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted_path: Option<String>,
    /// Bash 任务 id（只有后台/转后台路径存在）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// 进程退出码（只有进程型工具存在）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 本次调用耗时（毫秒）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// 工具自有字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl StructuredOutput {
    /// 新建带基线字段的成功形状。
    pub fn ok(tool: &str) -> Self {
        Self {
            tool: tool.to_string(),
            ok: true,
            truncated: false,
            persisted_path: None,
            task_id: None,
            exit_code: None,
            elapsed_ms: None,
            extra: BTreeMap::new(),
        }
    }

    /// 新建业务错误形状。
    pub fn error(tool: &str) -> Self {
        Self {
            ok: false,
            ..Self::ok(tool)
        }
    }

    /// 追加工具自有字段。
    pub fn with_extra(mut self, key: &str, value: serde_json::Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }
}

// ──────────────────────────────── 任务状态 DTO ───────────────────────────────

/// Bash 任务生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// 正在运行。
    Running,
    /// 自然或显式等待后正常结束。
    Completed,
    /// 结束但退出码非零。
    Failed,
    /// 请求取消终止，或进程组在 TERM→KILL 升级后仍未被回收（F-P3-01：显式
    /// [`crate::tasks::TaskRegistry::stop`] 与外部 `kill` 一般收敛为 [`TaskStatus::Failed`]，
    /// 只有拿不到退出码的不可回收路径才如实标 `Killed`）。
    Killed,
    /// 前台超时转后台或被超时终止。
    TimedOut,
    /// 已过 TTL / 超出保留条数，日志被回收。
    Gone,
}

impl TaskStatus {
    /// 是否为终态（终态不可再转移）。
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Bash 启动结果中的任务句柄。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskHandle {
    /// 任务 id：`shell-<UUIDv7>`。
    pub task_id: TaskId,
    /// 前台/后台进程 id（若已 spawn）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 进程组 id（`kill -- -pgid` 使用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// stdout 日志路径（workspace 或 server-private temp 内）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_log: Option<String>,
    /// stderr 日志路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<String>,
}

/// 任务快照：`sandbox://tasks` resources 的载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    /// 任务 id。
    pub task_id: TaskId,
    /// 归属主体（跨主体读取必须拒绝）。
    pub owner: PrincipalId,
    /// 归属连接实例。
    pub client_instance: ClientInstanceId,
    /// 当前状态。
    pub status: TaskStatus,
    /// 进程 id。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 进程组 id。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// stdout 日志路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_log: Option<String>,
    /// stderr 日志路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<String>,
    /// 退出码（终态时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 开始时间（RFC 3339）。
    pub started_at: String,
    /// 结束时间（RFC 3339）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

// ──────────────────────────────── 跨包 trait ────────────────────────────────

/// 跨 `.await` 传递的装箱 future 别名（保持 trait dyn 兼容）。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 工具执行 seam：MCP core 只依赖它，生产实现是进程内执行内核
/// [`crate::runtime::InProcessExecutor`]。
pub trait ToolExecutor: Send + Sync {
    /// 执行一次工具调用。
    ///
    /// 返回 `Err(ToolError)` 表示 JSON-RPC 协议错误；业务失败必须返回
    /// `Ok(ToolResponse { is_error: true, .. })`。
    fn execute<'a>(
        &'a self,
        request: ToolRequest,
    ) -> BoxFuture<'a, Result<ToolResponse, ToolError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_resolution_maps_only_known_names() {
        assert_eq!(resolve_tool_name("Read"), Some("Read"));
        assert_eq!(resolve_tool_name("reading"), Some("Read"));
        assert_eq!(resolve_tool_name("Shell"), Some("Bash"));
        assert_eq!(resolve_tool_name("Bash"), Some("Bash"));
        assert_eq!(
            resolve_tool_name("folder_operations"),
            Some("folder_operations")
        );
        assert_eq!(resolve_tool_name("NotAToolName"), None);
        assert_eq!(resolve_tool_name(""), None);
    }

    #[test]
    fn tool_names_are_unique_and_disjoint_from_aliases() {
        let names = tool_name_set();
        assert_eq!(names.len(), TOOL_NAMES.len());
        for (alias, canonical) in TOOL_ALIASES {
            assert!(!names.contains(alias), "别名 {alias} 不应作为工具条目出现");
            assert!(names.contains(canonical), "别名目标 {canonical} 必须存在");
        }
    }

    #[test]
    fn task_status_terminality() {
        assert!(!TaskStatus::Running.is_terminal());
        for status in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Killed,
            TaskStatus::TimedOut,
            TaskStatus::Gone,
        ] {
            assert!(status.is_terminal(), "{status:?} 应为终态");
        }
    }
}
