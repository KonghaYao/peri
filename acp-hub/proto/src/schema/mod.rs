//! Y.Doc schema 的 Rust 类型镜像（§5.3 / §5.4 / §5.5）。
//!
//! 承载 Chat / Control / Registry 三 Doc 的类型，**字段与枚举严格照抄架构
//! 文档 §5.3–5.5**。定位：字段名/枚举/嵌套关系的事实源 + 测试与调试用 serde
//! round-trip（镜像类型 derive Serialize/Deserialize，camelCase）；**不持有
//! yrs 句柄**——实际 yrs 读写由 server 聚合器经本模块导出的类型与字段常量
//! 完成（架构 §12：`server/src/state/chat-writer`）。
//!
//! 物理映射（§5.3 原文）：根对象/`entries`/`blocks`/`tool_calls` 用 `Y.Map`；
//! 顺序索引用 `Y.Array`（元素 `String`）；流式文本用 `Y.Text`；删除采用领域
//! tombstone，不由客户端物理删除权威记录。

mod chat;
mod registry;
mod control;

#[cfg(test)]
#[path = "schema_test.rs"]
mod schema_test;

pub use chat::{ChatDocRoot, ChatEntry, ContentBlock, ToolCallProjection};
pub use registry::{InstanceView, RegistryDocRoot, RegistryGlobal, ChatSummary};
pub use control::{
    ActiveTurnProjection, AgentStatusProjection, PermissionProjection,
    ControlDocRoot, ChatInfoProjection, SessionSummaryProjection,
};

use serde::{Deserialize, Serialize};

/// 脱敏公开错误（§9.3）：只允许稳定、脱敏的公开信息，不含内部细节。
///
/// 【决策】§9.3 仅规定「稳定错误码 + allowlist 摘要字段（状态/耗时/大小）」，
/// 字段集未展开；M1 最小实现 code + message，摘要字段随 §9.3 增补。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicError {
    /// 稳定错误码（如 `AGENT_UNAVAILABLE`，见 [`crate::ErrorCode`]）。
    pub code: String,
    /// 脱敏消息。
    pub message: String,
}

/// Chat Entry 类型（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    Message,
    Tool,
    System,
}

/// Chat Entry 角色（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryRole {
    User,
    Assistant,
    System,
}

/// Chat Entry 状态（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryStatus {
    Pending,
    Streaming,
    Completed,
    Cancelled,
    Error,
}

/// 内容块可见性（§5.3）：hidden 内容绝不发给无权客户端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockVisibility {
    Summary,
    Hidden,
}

/// 工具调用状态（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallStatus {
    Pending,
    AwaitingPermission,
    Running,
    Completed,
    Error,
    Cancelled,
}

/// 权限请求选项（§5.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionOptions {
    AllowOnce,
    AllowSession,
    Deny,
}

/// 权限请求状态（§5.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStatus {
    Pending,
    Resolved,
    Expired,
}

/// Turn 状态（§5.4 未展开枚举值域，【决策】按架构 §7.2 turn 状态机定稿）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Accepting,
    Running,
    AwaitingPermission,
    Cancelling,
    Completed,
    Cancelled,
    Interrupted,
    Failed,
}

/// Chat 状态（§5.4 未展开，【决策】按架构 §7.3 chat 生命周期定稿；
/// gap 独立字段承载，不进 status 枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatStatus {
    Accepting,
    Active,
    Ended,
    Closed,
    Crashed,
}

/// Instance 状态（§5.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstanceStatus {
    Online,
    Offline,
    Unknown,
}

/// Registry 全局状态（§5.5；Degraded 判定规则见架构 §17.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GlobalStatus {
    Healthy,
    Degraded,
    Restarting,
}
