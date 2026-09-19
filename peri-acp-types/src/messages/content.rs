use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Provider 原生历史载体的 type tag（OpenAI Responses）。
///
/// 载体形状为 `{"type": "responses_native_history", "history": <版本化记录>}`，经
/// [`ContentBlock::Unknown`] 透传：只有 provider adapter 才解码 `history`，其余层
/// （存储、投影、事件、遥测）把它当作不透明载荷处理。`history` 含 provider 私有
/// 状态（reasoning 密文与来源身份），不得出现在 Debug、摘要或遥测中。
pub const RESPONSES_NATIVE_HISTORY_TAG: &str = "responses_native_history";

/// 原生历史在可观测出口的占位载荷。
const RESPONSES_NATIVE_HISTORY_REDACTED: &str = "[REDACTED]";

// ─── ImageSource ──────────────────────────────────────────────────────────────

/// 图片数据来源
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ImageSource {
    /// Base64 编码的图片数据
    Base64 {
        media_type: String, // "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        data: String,
    },
    /// 远端 URL（OpenAI image_url 格式）
    Url { url: String },
}

// ─── DocumentSource ───────────────────────────────────────────────────────────

/// 文档数据来源
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum DocumentSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
    Text { text: String },
}

// ─── ContentBlock ─────────────────────────────────────────────────────────────

/// 标准 ContentBlock — 对齐 LangChain JS contentBlocks
///
/// 每个 variant 对应 LangChain 文档中的 Standard content block 类型。
#[derive(Clone, PartialEq)]
pub enum ContentBlock {
    /// 纯文本
    Text { text: String },

    /// 图片（多模态）
    Image { source: ImageSource },

    /// 文档（Anthropic Documents beta）
    Document {
        source: DocumentSource,
        title: Option<String>,
    },

    /// AI 发出的工具调用（server-side tool call）
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// 工具执行结果
    ToolResult {
        /// 内容块唯一标识（部分 provider 如 GLM Anthropic 兼容端口要求此字段）
        id: Option<String>,
        tool_use_id: String,
        content: Vec<ContentBlock>,
        is_error: bool,
    },

    /// 推理 / CoT 内容（Anthropic thinking / OpenAI reasoning）
    Reasoning {
        text: String,
        /// Anthropic extended thinking 签名（用于缓存校验）
        signature: Option<String>,
    },

    /// Provider 原生 block（透传，不做解析）
    ///
    /// 存储无法识别的原始 JSON，保证向前兼容。
    /// 携带完整原始数据，可在回传时保留全部字段。
    Unknown(serde_json::Value),
}

/// 手写 `Debug`：`Unknown` 可能是 provider 原生载荷，其中 Responses 原生历史含
/// reasoning 密文与来源身份（nonce/digest），`{:?}` 输出（含被 `format!` 拼接进
/// 日志/摘要/遥测的路径）不得泄漏这些字段。其余变体与 derive 输出保持一致。
impl fmt::Debug for ContentBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { text } => f.debug_struct("Text").field("text", text).finish(),
            Self::Image { source } => f.debug_struct("Image").field("source", source).finish(),
            Self::Document { source, title } => f
                .debug_struct("Document")
                .field("source", source)
                .field("title", title)
                .finish(),
            Self::ToolUse { id, name, input } => f
                .debug_struct("ToolUse")
                .field("id", id)
                .field("name", name)
                .field("input", input)
                .finish(),
            Self::ToolResult {
                id,
                tool_use_id,
                content,
                is_error,
            } => f
                .debug_struct("ToolResult")
                .field("id", id)
                .field("tool_use_id", tool_use_id)
                .field("content", content)
                .field("is_error", is_error)
                .finish(),
            Self::Reasoning { text, signature } => f
                .debug_struct("Reasoning")
                .field("text", text)
                .field("signature", signature)
                .finish(),
            Self::Unknown(value) => {
                let redacted = redacted_unknown_view(value);
                f.debug_tuple("Unknown")
                    .field(redacted.as_ref().unwrap_or(value))
                    .finish()
            }
        }
    }
}

/// 原生历史载体的脱敏视图；非原生历史返回 `None`（调用方原样输出）。
fn redacted_unknown_view(value: &serde_json::Value) -> Option<serde_json::Value> {
    if value.get("type").and_then(|t| t.as_str()) != Some(RESPONSES_NATIVE_HISTORY_TAG) {
        return None;
    }
    Some(serde_json::json!({
        "type": RESPONSES_NATIVE_HISTORY_TAG,
        "history": RESPONSES_NATIVE_HISTORY_REDACTED,
    }))
}

// ─── ContentBlock 手动 Serialize/Deserialize ──────────────────────────────────
//
// 不使用 #[serde(tag = "type")] derive，因为 Unknown 变体需要透传原始 JSON。
// derive 的 tag 模式无法在序列化时保留 Unknown 的完整数据。

impl Serialize for ContentBlock {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::Text { text } => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("type", "text")?;
                m.serialize_entry("text", text)?;
                m.end()
            }
            Self::Image { source } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("type", "image")?;
                m.serialize_entry("source", source)?;
                m.end()
            }
            Self::Document { source, title } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("type", "document")?;
                m.serialize_entry("source", source)?;
                if let Some(t) = title {
                    m.serialize_entry("title", t)?;
                }
                m.end()
            }
            Self::ToolUse { id, name, input } => {
                let mut m = s.serialize_map(Some(4))?;
                m.serialize_entry("type", "tool_use")?;
                m.serialize_entry("id", id)?;
                m.serialize_entry("name", name)?;
                m.serialize_entry("input", input)?;
                m.end()
            }
            Self::ToolResult {
                id,
                tool_use_id,
                content,
                is_error,
            } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("type", "tool_result")?;
                if let Some(id) = id {
                    m.serialize_entry("id", id)?;
                }
                m.serialize_entry("tool_use_id", tool_use_id)?;
                m.serialize_entry("content", content)?;
                m.serialize_entry("is_error", is_error)?;
                m.end()
            }
            Self::Reasoning { text, signature } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("type", "reasoning")?;
                m.serialize_entry("text", text)?;
                if let Some(sig) = signature {
                    m.serialize_entry("signature", sig)?;
                }
                m.end()
            }
            Self::Unknown(value) => value.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let type_str = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match type_str {
            "text" => {
                let text = value
                    .get("text")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("text"))?;
                Ok(Self::Text {
                    text: text.to_string(),
                })
            }
            "image" => {
                let source =
                    serde_json::from_value(value.get("source").cloned().unwrap_or_default())
                        .map_err(|e| serde::de::Error::custom(format!("invalid source: {}", e)))?;
                Ok(Self::Image { source })
            }
            "document" => {
                let source =
                    serde_json::from_value(value.get("source").cloned().unwrap_or_default())
                        .map_err(|e| serde::de::Error::custom(format!("invalid source: {}", e)))?;
                let title = value
                    .get("title")
                    .and_then(|t| t.as_str())
                    .map(String::from);
                Ok(Self::Document { source, title })
            }
            "tool_use" => {
                let id = value
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("id"))?
                    .to_string();
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("name"))?
                    .to_string();
                let input = value
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                Ok(Self::ToolUse { id, name, input })
            }
            "tool_result" => {
                let id = value.get("id").and_then(|v| v.as_str()).map(String::from);
                let tool_use_id = value
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("tool_use_id"))?
                    .to_string();
                let content: Vec<ContentBlock> = value
                    .get("content")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| serde::de::Error::custom(format!("invalid content: {}", e)))?
                    .unwrap_or_default();
                let is_error = value
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Ok(Self::ToolResult {
                    id,
                    tool_use_id,
                    content,
                    is_error,
                })
            }
            "reasoning" => {
                let text = value
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("text"))?
                    .to_string();
                let signature = value
                    .get("signature")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                Ok(Self::Reasoning { text, signature })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn image_url(url: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource::Url { url: url.into() },
        }
    }

    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource::Base64 {
                media_type: media_type.into(),
                data: data.into(),
            },
        }
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
        content: Vec<ContentBlock>,
        is_error: bool,
    ) -> Self {
        Self::ToolResult {
            id: None,
            tool_use_id: tool_use_id.into(),
            content,
            is_error,
        }
    }

    pub fn reasoning(text: impl Into<String>) -> Self {
        Self::Reasoning {
            text: text.into(),
            signature: None,
        }
    }

    pub fn reasoning_with_signature(text: impl Into<String>, signature: impl Into<String>) -> Self {
        Self::Reasoning {
            text: text.into(),
            signature: Some(signature.into()),
        }
    }

    /// 若是 TextBlock，返回文字内容
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }

    /// 若是 ToolUse，返回 (id, name, input)
    pub fn as_tool_use(&self) -> Option<(&str, &str, &serde_json::Value)> {
        match self {
            Self::ToolUse { id, name, input } => Some((id, name, input)),
            _ => None,
        }
    }

    /// 若是 Reasoning，返回文字
    pub fn as_reasoning(&self) -> Option<&str> {
        match self {
            Self::Reasoning { text, .. } => Some(text),
            _ => None,
        }
    }

    /// 构造 Responses 原生历史载体。
    ///
    /// 载荷不解释、不校验：版本与记录合法性由 provider adapter 解码时判定。
    pub fn responses_native_history(history: serde_json::Value) -> Self {
        Self::Unknown(serde_json::json!({
            "type": RESPONSES_NATIVE_HISTORY_TAG,
            "history": history,
        }))
    }

    /// 是否为 Responses 原生历史载体（仅按确定 tag 判定）。
    pub fn is_responses_native_history(&self) -> bool {
        matches!(
            self,
            Self::Unknown(value)
                if value.get("type").and_then(|t| t.as_str()) == Some(RESPONSES_NATIVE_HISTORY_TAG)
        )
    }

    /// Responses 原生历史的 `history` 载荷；非原生历史或载荷缺失返回 `None`。
    pub fn responses_native_history_payload(&self) -> Option<&serde_json::Value> {
        if !self.is_responses_native_history() {
            return None;
        }
        let Self::Unknown(value) = self else {
            return None;
        };
        value.get("history")
    }

    /// 可观测投影：原生历史载荷替换为固定占位，其余 block 原样保留。
    ///
    /// 用于事件、遥测与 LLM 摘要输入等出口——这些出口只需要用户可见内容，不应携带
    /// provider 私有状态（reasoning 密文与来源身份）。持久化、工具执行与模型回放必须
    /// 使用原 block，不能以本投影替代。
    pub fn redacted_for_observability(&self) -> Self {
        if self.is_responses_native_history() {
            Self::Unknown(serde_json::json!({
                "type": RESPONSES_NATIVE_HISTORY_TAG,
                "history": RESPONSES_NATIVE_HISTORY_REDACTED,
            }))
        } else {
            self.clone()
        }
    }
}

// ─── MessageContent ────────────────────────────────────────────────────────────

/// 消息内容 — 对齐 LangChain JS content 属性
///
/// 支持三种形式（与 LangChain JS 文档一一对应）：
///
/// 1. `String`                  — 纯文本（最常见）
/// 2. `Blocks(Vec<ContentBlock>)` — 标准 ContentBlock 列表（跨 provider 兼容）
/// 3. `Raw(Vec<serde_json::Value>)` — Provider 原生格式（透传）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    /// 纯文本
    Text(String),
    /// 标准 content blocks（type-safe）
    Blocks(Vec<ContentBlock>),
    /// Provider 原生格式（raw JSON objects）
    Raw(Vec<serde_json::Value>),
}

impl MessageContent {
    /// 从字符串构建
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// 从 ContentBlock 列表构建
    pub fn blocks(blocks: Vec<ContentBlock>) -> Self {
        Self::Blocks(blocks)
    }

    /// 从 provider 原生 JSON 列表构建
    pub fn raw(values: Vec<serde_json::Value>) -> Self {
        Self::Raw(values)
    }

    /// 提取所有文本内容（拼接多个 text block）
    pub fn text_content(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| b.as_text())
                .collect::<Vec<_>>()
                .join(""),
            Self::Raw(values) => values
                .iter()
                .filter(|v| v["type"].as_str() == Some("text"))
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    /// 懒解析为标准 ContentBlock 列表（对齐 LangChain JS `contentBlocks` 属性）
    ///
    /// - `Text(s)` → `[ContentBlock::Text { text: s }]`
    /// - `Blocks(v)` → 直接返回
    /// - `Raw(v)` → 尝试按 type 字段解析为已知 block
    pub fn content_blocks(&self) -> Vec<ContentBlock> {
        match self {
            Self::Text(s) => {
                if s.is_empty() {
                    vec![]
                } else {
                    vec![ContentBlock::text(s.clone())]
                }
            }
            Self::Blocks(blocks) => blocks.clone(),
            Self::Raw(values) => values
                .iter()
                .map(|v| {
                    serde_json::from_value::<ContentBlock>(v.clone())
                        .unwrap_or_else(|_| ContentBlock::Unknown(v.clone()))
                })
                .collect(),
        }
    }

    /// 是否为空内容
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(s) => s.is_empty(),
            Self::Blocks(b) => b.is_empty(),
            Self::Raw(v) => v.is_empty(),
        }
    }

    /// 是否包含工具调用 block
    pub fn has_tool_use(&self) -> bool {
        self.content_blocks()
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }

    /// 是否携带 Responses 原生历史载体。
    pub fn has_provider_native_history(&self) -> bool {
        self.content_blocks()
            .iter()
            .any(ContentBlock::is_responses_native_history)
    }

    /// 可观测投影：逐 block 应用 [`ContentBlock::redacted_for_observability`]。
    ///
    /// 文本/图片/工具等用户内容不变，provider 私有状态被剥离；不含原生历史时原样
    /// 返回，避免改变既有 variant。
    pub fn redacted_for_observability(&self) -> Self {
        if !self.has_provider_native_history() {
            return self.clone();
        }
        Self::Blocks(
            self.content_blocks()
                .iter()
                .map(ContentBlock::redacted_for_observability)
                .collect(),
        )
    }

    /// 提取所有 ToolUse blocks（覆盖 Text/Blocks/Raw 三种变体）
    pub fn tool_use_blocks(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        match self {
            Self::Blocks(blocks) => blocks.iter().filter_map(|b| b.as_tool_use()).collect(),
            Self::Raw(values) => values
                .iter()
                .filter_map(|v| {
                    if v["type"].as_str() == Some("tool_use") {
                        let id = v["id"].as_str()?;
                        let name = v["name"].as_str()?;
                        let input = v.get("input")?;
                        Some((id, name, input))
                    } else {
                        None
                    }
                })
                .collect(),
            _ => vec![],
        }
    }
}

impl Default for MessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<&str> for MessageContent {
    fn from(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}

impl From<Vec<ContentBlock>> for MessageContent {
    fn from(blocks: Vec<ContentBlock>) -> Self {
        Self::Blocks(blocks)
    }
}

// ─── 文本清洗 ─────────────────────────────────────────────────────────────────

/// 仅剥离精确、无属性的 legacy `<system-reminder>` 块。
///
/// canonical/future XML 可能是用户粘贴内容；在调用方迁移到结构化 reminder 前，默认
/// facade 必须保留其原文。未闭合 legacy 标签也原样保留。
pub fn strip_system_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";

    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        let Some(close) = after_open.find(CLOSE) else {
            output.push_str(&rest[start..]);
            return output;
        };
        rest = &after_open[close + CLOSE.len()..];
    }
    output.push_str(rest);
    output
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "content_test.rs"]
mod tests;
