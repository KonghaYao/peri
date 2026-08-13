//! Chat Doc 类型镜像（§5.3，`CHAT_DOC_SCHEMA_VERSION = 1`）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{BlockVisibility, EntryKind, EntryRole, EntryStatus, PublicError, ToolCallStatus};

/// Chat Doc 根对象（§5.3）。
///
/// 无 `committed_commands`：去重记录在 server command outbox（§4.4），不随
/// Doc 生命周期存亡（架构顾问 P0-1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDocRoot {
    /// == [`crate::version::CHAT_DOC_SCHEMA_VERSION`]。
    pub schema_version: u32,
    /// 每次成功投影 +1；与 schema_version 分离（§5.6）。
    pub projection_version: u32,
    /// `Y.Array<String>`，与 entries 分离便于局部更新/未来分页。
    pub entry_order: Vec<String>,
    pub entries: HashMap<String, ChatEntry>,
    pub tool_calls: HashMap<String, ToolCallProjection>,
}

/// Chat Entry（§5.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEntry {
    /// 派生规则：`{turnId}:user` / `{turnId}:assistant` / tool: 按 toolCallId。
    pub entry_id: String,
    pub turn_id: Option<String>,
    pub kind: EntryKind,
    pub role: EntryRole,
    pub status: EntryStatus,
    pub author_user_id: Option<String>,
    /// RFC3339。
    pub created_at: String,
    pub completed_at: Option<String>,
    /// `Y.Array<String>`。
    pub block_order: Vec<String>,
    pub blocks: HashMap<String, ContentBlock>,
    /// 脱敏公开错误，不含内部细节。
    pub error: Option<PublicError>,
}

/// 内容块（§5.3）。镜像内部判别形态（tag `"kind"`），非线协议。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentBlock {
    /// 流式文本用 Y.Text（避免每 token 替换完整字符串）。
    Text { block_id: String, text: String },
    /// hidden 内容绝不发给无权客户端（§5.3）。
    Reasoning {
        block_id: String,
        text: String,
        visibility: BlockVisibility,
    },
    ToolCall {
        block_id: String,
        tool_call_id: String,
    },
    /// 只存引用，不嵌入内容。
    Resource {
        block_id: String,
        resource_id: String,
        media_type: String,
        name: String,
    },
}

/// 工具调用投影（§5.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallProjection {
    pub tool_call_id: String,
    pub turn_id: String,
    pub name: String,
    pub status: ToolCallStatus,
    /// 过滤内部/敏感字段后投影。
    pub arguments: Option<serde_json::Value>,
    /// 超大结果仅保留受授权资源引用。
    pub result: Option<serde_json::Value>,
    pub public_error: Option<PublicError>,
    pub permission_id: Option<String>,
}
