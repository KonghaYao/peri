use serde::{Deserialize, Serialize};

/// 消息唯一标识符 — UUID v7（时间有序，跨进程安全）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(uuid::Uuid);

impl MessageId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl From<uuid::Uuid> for MessageId {
    fn from(u: uuid::Uuid) -> Self {
        Self(u)
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

use super::content::{ContentBlock, MessageContent};

// ─── ToolCallRequest ──────────────────────────────────────────────────────────

/// 工具调用请求（对应 OpenAI tool_calls / Anthropic tool_use blocks）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ToolCallRequest {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

// ─── BaseMessage ──────────────────────────────────────────────────────────────

/// BaseMessage - 统一消息类型，对齐 LangChain BaseMessage
///
/// `content` 字段为 `MessageContent`，支持：
/// - 纯文本字符串
/// - 标准 ContentBlock 列表（多模态、推理内容等）
/// - Provider 原生格式（透传）
///
/// `content_blocks()` 方法懒解析，对齐 LangChain JS 的 `contentBlocks` 属性。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum BaseMessage {
    #[serde(rename = "user")]
    Human {
        id: MessageId,
        content: MessageContent,
    },

    #[serde(rename = "assistant")]
    Ai {
        id: MessageId,
        content: MessageContent,
        /// P1-6: tool_calls 是从 ContentBlock::ToolUse 派生的只读缓存。
        ///
        /// 同一个 tool use 在 message 中以两种形式存在：
        /// 1. `content` 中的 `ContentBlock::ToolUse` 块（规范来源）
        /// 2. `tool_calls: Vec<ToolCallRequest>` 派生字段（便利查询）
        ///
        /// `has_tool_calls()` 和 `tool_calls()` 都以此字段为准——确保两者始终同步。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCallRequest>,
    },

    #[serde(rename = "system")]
    System {
        id: MessageId,
        content: MessageContent,
    },

    #[serde(rename = "tool")]
    Tool {
        id: MessageId,
        tool_call_id: String,
        content: MessageContent,
        #[serde(default)]
        is_error: bool,
    },
}

impl BaseMessage {
    // ── 构造器 ────────────────────────────────────────────────────────────────

    pub fn human(content: impl Into<MessageContent>) -> Self {
        Self::Human {
            id: MessageId::new(),
            content: content.into(),
        }
    }

    pub fn ai(content: impl Into<MessageContent>) -> Self {
        Self::Ai {
            id: MessageId::new(),
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }

    pub fn ai_with_tool_calls(
        content: impl Into<MessageContent>,
        tool_calls: Vec<ToolCallRequest>,
    ) -> Self {
        Self::Ai {
            id: MessageId::new(),
            content: content.into(),
            tool_calls,
        }
    }

    /// 构造带 ContentBlock 列表的 AI 消息（含工具调用 block）
    ///
    /// `blocks` 中的 `ToolUse` block 会被同步提取到 `tool_calls`，保持一致性。
    pub fn ai_from_blocks(blocks: Vec<ContentBlock>) -> Self {
        let tool_calls: Vec<ToolCallRequest> = blocks
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolUse { id, name, input } = b {
                    Some(ToolCallRequest::new(
                        id.clone(),
                        name.clone(),
                        input.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        Self::Ai {
            id: MessageId::new(),
            content: MessageContent::Blocks(blocks),
            tool_calls,
        }
    }

    pub fn system(content: impl Into<MessageContent>) -> Self {
        Self::System {
            id: MessageId::new(),
            content: content.into(),
        }
    }

    pub fn tool_result(id: impl Into<String>, content: impl Into<MessageContent>) -> Self {
        Self::Tool {
            id: MessageId::new(),
            tool_call_id: id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn tool_error(id: impl Into<String>, error: impl Into<MessageContent>) -> Self {
        Self::Tool {
            id: MessageId::new(),
            tool_call_id: id.into(),
            content: error.into(),
            is_error: true,
        }
    }

    // ── 访问器 ────────────────────────────────────────────────────────────────

    /// 以指定 ID 替换消息身份（流式发射路径用：chunk 的 messageId 与
    /// transcript 中定型消息的 ID 对齐，保证 wire 上 messageId 即规范消息 ID）。
    pub fn with_message_id(mut self, id: MessageId) -> Self {
        match &mut self {
            Self::Human { id: mid, .. }
            | Self::Ai { id: mid, .. }
            | Self::System { id: mid, .. }
            | Self::Tool { id: mid, .. } => *mid = id,
        }
        self
    }

    /// 获取消息 ID
    pub fn id(&self) -> MessageId {
        match self {
            Self::Human { id, .. } => *id,
            Self::Ai { id, .. } => *id,
            Self::System { id, .. } => *id,
            Self::Tool { id, .. } => *id,
        }
    }

    /// 获取消息 `MessageContent` 引用
    pub fn message_content(&self) -> &MessageContent {
        match self {
            Self::Human { content, .. } => content,
            Self::Ai { content, .. } => content,
            Self::System { content, .. } => content,
            Self::Tool { content, .. } => content,
        }
    }

    /// 获取纯文本内容（拼接所有 text block）
    pub fn content(&self) -> String {
        self.message_content().text_content()
    }

    /// 懒解析为标准 ContentBlock 列表
    ///
    /// 对齐 LangChain JS 的 `message.contentBlocks` 属性。
    pub fn content_blocks(&self) -> Vec<ContentBlock> {
        self.message_content().content_blocks()
    }

    /// 是否包含工具调用（P1-6: 仅检查 `tool_calls` 派生字段，与 ContentBlock::ToolUse 块同步）
    pub fn has_tool_calls(&self) -> bool {
        match self {
            Self::Ai { tool_calls, .. } => !tool_calls.is_empty(),
            _ => false,
        }
    }

    /// 获取工具调用列表（仅 Ai 变体有效）
    ///
    /// P1-6: 返回 `tool_calls` 派生字段，与 `content_blocks()` 中的 `ContentBlock::ToolUse` 块同步。
    /// LLM 适配器层负责保持两者一致。
    pub fn tool_calls(&self) -> &[ToolCallRequest] {
        match self {
            Self::Ai { tool_calls, .. } => tool_calls,
            _ => &[],
        }
    }

    /// 是否为系统消息
    pub fn is_system(&self) -> bool {
        matches!(self, Self::System { .. })
    }

    /// 克隆消息但替换 content 字段
    pub fn clone_with_content(&self, content: MessageContent) -> Self {
        match self {
            Self::Human { id, .. } => Self::Human { id: *id, content },
            Self::Ai { id, tool_calls, .. } => Self::Ai {
                id: *id,
                content,
                tool_calls: tool_calls.clone(),
            },
            Self::System { id, .. } => Self::System { id: *id, content },
            Self::Tool {
                id,
                tool_call_id,
                is_error,
                ..
            } => Self::Tool {
                id: *id,
                tool_call_id: tool_call_id.clone(),
                content,
                is_error: *is_error,
            },
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "message_test.rs"]
mod tests;
