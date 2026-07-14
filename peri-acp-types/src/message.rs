//! Message DTOs -- TUI 渲染所需的 BaseMessage / ContentBlock 等。
//!
//! 这些 DTO 与 `peri_agent::messages` 中的类型**结构等价**，
//! 但只保留 TUI 真正消费的字段（去掉了 role-specific metadata）。
//! 转换在 acp_server/view_mapper.rs 完成。

use serde::{Deserialize, Serialize};

/// 消息唯一标识符 -- UUID v7（时间有序，跨进程安全）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageIdDto(uuid::Uuid);

impl MessageIdDto {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    pub fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for MessageIdDto {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum BaseMessageDto {
    Human(HumanMessageData),
    Ai(AiMessageData),
    System(SystemMessageData),
    Tool(ToolMessageData),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HumanMessageData {
    pub content: MessageContentDto,
    pub message_id: MessageIdDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiMessageData {
    pub content: MessageContentDto,
    pub message_id: MessageIdDto,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRequestDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemMessageData {
    pub content: MessageContentDto,
    pub message_id: MessageIdDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolMessageData {
    pub tool_call_id: String,
    pub content: MessageContentDto,
    pub message_id: MessageIdDto,
    #[serde(default)]
    pub is_error: bool,
}

/// 工具调用请求（AI 消息携带）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRequestDto {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContentDto {
    Text(String),
    Blocks(Vec<ContentBlockDto>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentBlockDto {
    Text {
        text: String,
    },
    Image {
        source: ImageSourceDto,
    },
    Document {
        source: DocumentSourceDto,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Reasoning {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Unknown {
        raw: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum ImageSourceDto {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum DocumentSourceDto {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

// ─── 构造器 ────────────────────────────────────────────────────────────────

impl BaseMessageDto {
    pub fn human(text: impl Into<String>) -> Self {
        Self::Human(HumanMessageData {
            content: MessageContentDto::Text(text.into()),
            message_id: MessageIdDto::new(),
        })
    }

    pub fn ai(text: impl Into<String>) -> Self {
        Self::Ai(AiMessageData {
            content: MessageContentDto::Text(text.into()),
            message_id: MessageIdDto::new(),
            tool_calls: vec![],
        })
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::Tool(ToolMessageData {
            tool_call_id: tool_call_id.into(),
            content: MessageContentDto::Text(content.into()),
            message_id: MessageIdDto::new(),
            is_error,
        })
    }
}

// ─── 访问器（对齐 peri_agent::messages::BaseMessage 方法签名） ────────────

impl BaseMessageDto {
    /// 获取消息 ID
    pub fn id(&self) -> MessageIdDto {
        match self {
            Self::Human(d) => d.message_id,
            Self::Ai(d) => d.message_id,
            Self::System(d) => d.message_id,
            Self::Tool(d) => d.message_id,
        }
    }

    /// 获取纯文本内容（拼接所有 text block）
    pub fn content(&self) -> String {
        self.message_content().text_content()
    }

    /// 获取 MessageContent 引用
    pub fn message_content(&self) -> &MessageContentDto {
        match self {
            Self::Human(d) => &d.content,
            Self::Ai(d) => &d.content,
            Self::System(d) => &d.content,
            Self::Tool(d) => &d.content,
        }
    }

    /// 解析为标准 ContentBlock 列表
    pub fn content_blocks(&self) -> Vec<ContentBlockDto> {
        self.message_content().content_blocks()
    }

    /// 获取工具调用列表（仅 Ai 变体有效）
    pub fn tool_calls(&self) -> &[ToolCallRequestDto] {
        match self {
            Self::Ai(d) => &d.tool_calls,
            _ => &[],
        }
    }

    /// 是否包含工具调用
    pub fn has_tool_calls(&self) -> bool {
        match self {
            Self::Ai(d) => !d.tool_calls.is_empty(),
            _ => false,
        }
    }

    /// 是否为系统消息
    pub fn is_system(&self) -> bool {
        matches!(self, Self::System(_))
    }
}

impl MessageContentDto {
    /// 获取纯文本内容（拼接所有 text block）
    pub fn text_content(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlockDto::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    /// 解析为标准 ContentBlock 列表
    pub fn content_blocks(&self) -> Vec<ContentBlockDto> {
        match self {
            Self::Text(s) => vec![ContentBlockDto::text(s)],
            Self::Blocks(blocks) => blocks.clone(),
        }
    }
}

impl ContentBlockDto {
    pub fn text(t: impl Into<String>) -> Self {
        Self::Text { text: t.into() }
    }

    pub fn tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    pub fn tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error,
        }
    }

    pub fn reasoning(thinking: impl Into<String>, signature: impl Into<Option<String>>) -> Self {
        Self::Reasoning {
            thinking: thinking.into(),
            signature: signature.into(),
        }
    }
}
