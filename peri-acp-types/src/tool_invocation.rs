//! canonical 工具执行的身份与来源契约。
//!
//! **两个身份必须分开**：
//! - [`InvocationId`]：一次 canonical 执行的身份（可能由模型、App 宿主、Host 或
//!   嵌套调用发起）；出现在 ACP/App 线上协议的
//!   `_meta["peri.invocation"].invocationId`。
//! - [`ToolCallId`]：模型 tool-call ID，只负责与原始模型消息 / 助手卡片配对。
//!
//! [`InvocationOrigin`] 类型化表达「谁发起了这次执行」。调用方不得再用字符串
//! 前缀（`starts_with("mcp-app:")`）或 ID 拼接（`"{parent}/{invocation}"`）
//! 推断来源与嵌套关系。
//!
//! 本模块类型经 [`crate::tools`] re-export，消费方沿用 `tools::InvocationId` 等路径。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 一次 canonical 工具执行的稳定身份。
///
/// 与模型 tool-call ID（[`ToolCallId`]）是**两个不同的身份**，不得互相替代：
/// 本类型标识一次 canonical 执行（可能来自模型、App 宿主、Host 或嵌套调用），
/// 模型 tool-call ID 只负责与原始模型消息 / 助手卡片配对。
///
/// 字符串形状见 ACP/App 线上协议的 `_meta["peri.invocation"].invocationId`；
/// 同一执行跨进程重放保持不变。新执行身份由 [`InvocationId::generate`] 生成。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InvocationId(String);

impl InvocationId {
    /// 生成新的执行身份（UUID v7，时间有序）。
    pub fn generate() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for InvocationId {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}

impl From<String> for InvocationId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for InvocationId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl AsRef<str> for InvocationId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// 模型 tool-call ID（模型 assistant 消息 `tool_calls[].id` 与 tool 结果消息
/// `tool_call_id` 的配对键）。
///
/// 类型化只为阻止与 [`InvocationId`] 混用；字符串形状即模型 API 下发值，不做归一化。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ToolCallId {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}

impl From<String> for ToolCallId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ToolCallId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl AsRef<str> for ToolCallId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// 一次 canonical 执行的来源。
///
/// 来源是执行身份的一部分，必须类型化表达：调用方不得再从 ID 文本推断来源
/// （旧实现靠 `invocation_id.starts_with("mcp-app:")` 判定 App 调用、用
/// `"{parent}/{invocation}"` 拼接嵌套 ID）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum InvocationOrigin {
    /// 模型 tool call 直接发起。
    Model {
        /// 与该执行对应的模型 tool-call ID。
        tool_call_id: ToolCallId,
    },
    /// 由另一次 canonical 执行嵌套发起（执行中再次调用工具）。
    Nested {
        /// 外层 canonical 执行身份；事件投影据此还原嵌套链。
        parent_invocation_id: InvocationId,
    },
    /// MCP App 宿主发起的执行（App 内 rerun / App 工具调用）。
    App {
        /// App 实例身份（当前对应 `McpAppRecord::record_id`）。
        app_id: String,
        /// 承载本次执行的连接附件；`None` = record 已签发但 App 尚未附着连接
        /// （无路由 endpoint，交互归属因此 fail-closed）。
        ///
        /// 附件身份由 `SessionInteractionRouter` 分配（阶段 4a），与 record 身份
        /// **分离**：删除旧 lease 权威（`AppSessionBinding`）后，两者不再同源。
        attachment_id: Option<crate::interaction::AttachmentId>,
        /// 该 Attachment 的连接纪元；与 [`Self::attachment_id`] 同生共死
        /// （`None` = 尚未附着）。
        connection_epoch: Option<crate::interaction::ConnectionEpoch>,
        /// 服务端世代：服务端重连/重启后递增，用于拒绝陈旧 record 发起的执行。
        server_generation: u64,
    },
    /// Host 程序化调用（宿主入口直接发起的执行）。
    Host {
        /// 宿主入口命名空间（如 ACP `peri/mcp/invoke`）。
        namespace: String,
        /// 同一 namespace 内单调递增的调用序号。
        seq: u64,
    },
}

impl InvocationOrigin {
    /// 模型发起时返回该执行的模型 tool-call ID。
    pub fn model_tool_call_id(&self) -> Option<&ToolCallId> {
        match self {
            Self::Model { tool_call_id } => Some(tool_call_id),
            _ => None,
        }
    }

    /// 嵌套发起时返回外层执行身份。
    pub fn parent_invocation_id(&self) -> Option<&InvocationId> {
        match self {
            Self::Nested {
                parent_invocation_id,
            } => Some(parent_invocation_id),
            _ => None,
        }
    }

    /// 是否由 MCP App 发起（取代字符串前缀判定）。
    pub fn is_app(&self) -> bool {
        matches!(self, Self::App { .. })
    }
}

/// 可由宿主工具发起的 canonical effective-tool 调用。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveToolCall {
    pub invocation_id: InvocationId,
    pub tool_name: String,
    pub input: serde_json::Value,
    /// 本次执行的来源；嵌套调用以 [`InvocationOrigin::Nested`] 表达（取代原
    /// `parent_invocation_id: Option<String>` 字段）。
    pub origin: InvocationOrigin,
}

#[cfg(test)]
#[path = "tool_invocation_test.rs"]
mod tests;
