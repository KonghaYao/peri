use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::responses_history::ResponsesHistoryV1;
use crate::{ModelError, ModelResult, ProtocolErrorKind};

/// 受约束的 JSON object，用于工具参数和 JSON Schema。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JsonObject(BTreeMap<String, Value>);

impl JsonObject {
    pub fn new(fields: BTreeMap<String, Value>) -> Self {
        Self(fields)
    }

    pub fn from_value(value: Value) -> ModelResult<Self> {
        let Value::Object(fields) = value else {
            return Err(ModelError::protocol(ProtocolErrorKind::InvalidJsonObject));
        };
        Ok(Self(fields.into_iter().collect()))
    }

    pub fn as_map(&self) -> &BTreeMap<String, Value> {
        &self.0
    }
}

impl From<BTreeMap<String, Value>> for JsonObject {
    fn from(fields: BTreeMap<String, Value>) -> Self {
        Self::new(fields)
    }
}

/// 媒体数据的 IANA media type。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MediaType(String);

impl MediaType {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 图片内容的数据来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: MediaType, data: String },
    Url { url: String },
}

/// 文档内容的数据来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocumentSource {
    Base64 { media_type: MediaType, data: String },
    Url { url: String },
    Text { text: String },
}

/// 与 provider 无关的内容块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    Document {
        source: DocumentSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        tool_call: ToolCall,
    },
    ToolResult {
        result: Box<ToolResult>,
    },
    /// Provider 已显式标记为不可见的推理内容。
    RedactedReasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    /// Responses 原生 output items（message/reasoning/function_call）的完整记录。
    ///
    /// 该 block 是原生事实的唯一载体，用于同 endpoint/model 域的无状态续轮；同一消息中
    /// 的文本/推理/工具调用是它的派生视图，回放时以本 block 为准。跨协议或跨来源时
    /// 其他 adapter 必须丢弃本 block（派生视图仍可回放），不得回放其中的 reasoning 密文。
    ResponsesNativeHistory {
        history: Box<ResponsesHistoryV1>,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn reasoning(text: impl Into<String>) -> Self {
        Self::Reasoning {
            text: text.into(),
            signature: None,
        }
    }

    pub fn text_content(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            // 原生记录含派生文本，但文本由同消息的 `Text` block 承载；此处返回 `None`
            // 避免同一段答案被计两次。
            Self::Image { .. }
            | Self::Document { .. }
            | Self::Reasoning { .. }
            | Self::ToolUse { .. }
            | Self::ToolResult { .. }
            | Self::RedactedReasoning { .. }
            | Self::ResponsesNativeHistory { .. } => None,
        }
    }
}

/// 提供给模型的工具定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: JsonObject,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, input_schema: JsonObject) -> Self {
        Self {
            name: name.into(),
            description: None,
            input_schema,
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// 模型发起的结构化工具调用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    id: String,
    name: String,
    arguments: JsonObject,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: JsonObject) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments(&self) -> &JsonObject {
        &self.arguments
    }
}

/// 工具调用的执行结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub tool_call_id: String,
    pub name: String,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: vec![ContentBlock::text(content)],
            is_error: false,
        }
    }

    pub fn error(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: vec![ContentBlock::text(content)],
            is_error: true,
        }
    }

    pub fn is_success(&self) -> bool {
        !self.is_error
    }
}

/// 标准对话消息。Assistant 可同时包含内容与工具调用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ModelMessage {
    System {
        content: Vec<ContentBlock>,
    },
    User {
        content: Vec<ContentBlock>,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<ContentBlock>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        result: ToolResult,
    },
}

impl ModelMessage {
    pub fn system_text(text: impl Into<String>) -> Self {
        Self::System {
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::User {
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn assistant(content: Vec<ContentBlock>, tool_calls: Vec<ToolCall>) -> Self {
        Self::Assistant {
            content,
            tool_calls,
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::assistant(vec![ContentBlock::text(text)], Vec::new())
    }

    pub fn tool_result(result: ToolResult) -> Self {
        Self::ToolResult { result }
    }

    pub fn text_content(&self) -> Option<String> {
        match self {
            Self::System { content } | Self::User { content } | Self::Assistant { content, .. } => {
                Some(
                    content
                        .iter()
                        .filter_map(ContentBlock::text_content)
                        .collect(),
                )
            }
            Self::ToolResult { result } => Some(
                result
                    .content
                    .iter()
                    .filter_map(ContentBlock::text_content)
                    .collect(),
            ),
        }
    }

    pub fn is_assistant(&self) -> bool {
        matches!(self, Self::Assistant { .. })
    }
}

/// 标准模型请求。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub messages: Vec<ModelMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl ModelRequest {
    pub fn new(messages: Vec<ModelMessage>) -> Self {
        Self {
            messages,
            ..Self::default()
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// 标准模型响应，仅允许 Assistant 消息。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelResponse {
    message: ModelMessage,
    stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

#[derive(Deserialize)]
struct ModelResponseWire {
    message: ModelMessage,
    stop_reason: StopReason,
    #[serde(default)]
    usage: Option<TokenUsage>,
    #[serde(default)]
    request_id: Option<String>,
}

impl<'de> Deserialize<'de> for ModelResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ModelResponseWire::deserialize(deserializer)?;
        Self::new(wire.message, wire.stop_reason, wire.usage, wire.request_id)
            .map_err(serde::de::Error::custom)
    }
}

impl ModelResponse {
    pub fn new(
        message: ModelMessage,
        stop_reason: StopReason,
        usage: Option<TokenUsage>,
        request_id: Option<String>,
    ) -> ModelResult<Self> {
        if !message.is_assistant() {
            return Err(ModelError::protocol(
                ProtocolErrorKind::AssistantMessageRequired,
            ));
        }
        Ok(Self {
            message,
            stop_reason,
            usage,
            request_id,
        })
    }

    pub fn message(&self) -> &ModelMessage {
        &self.message
    }

    pub fn stop_reason(&self) -> &StopReason {
        &self.stop_reason
    }

    pub fn usage(&self) -> Option<&TokenUsage> {
        self.usage.as_ref()
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub fn assistant_text(&self) -> Option<String> {
        self.message.text_content()
    }

    pub(crate) fn set_text_if_empty(&mut self, text: String) {
        if text.is_empty() || self.assistant_text().is_some_and(|value| !value.is_empty()) {
            return;
        }
        if let ModelMessage::Assistant { content, .. } = &mut self.message {
            content.push(ContentBlock::text(text));
        }
    }

    pub(crate) fn set_reasoning_if_empty(&mut self, text: String) {
        if text.is_empty()
            || matches!(
                &self.message,
                ModelMessage::Assistant { content, .. }
                    if content.iter().any(|block| matches!(block, ContentBlock::Reasoning { .. }))
            )
        {
            return;
        }
        if let ModelMessage::Assistant { content, .. } = &mut self.message {
            content.push(ContentBlock::reasoning(text));
        }
    }

    pub(crate) fn set_tool_calls_if_empty(&mut self, tool_calls: Vec<ToolCall>) {
        if tool_calls.is_empty() {
            return;
        }
        if let ModelMessage::Assistant {
            tool_calls: response_tool_calls,
            ..
        } = &mut self.message
        {
            if response_tool_calls.is_empty() {
                *response_tool_calls = tool_calls;
            }
        }
    }

    pub(crate) fn set_usage_if_none(&mut self, usage: Option<TokenUsage>) {
        if self.usage.is_none() {
            self.usage = usage;
        }
    }

    /// 累加失败尝试已确认的真实用量（不覆盖 provider 上报的最终用量）。
    pub(crate) fn accumulate_usage(&mut self, extra: Option<TokenUsage>) {
        if let Some(extra) = extra {
            TokenUsage::accumulate(&mut self.usage, extra);
        }
    }
}

/// 模型调用的 token 使用量。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

impl TokenUsage {
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            ..Self::default()
        }
    }

    /// 把一次真实消耗并入住调用级用量（`None` = 尚无已确认用量）。
    ///
    /// 一次调用可能经历多次 HTTP 尝试：失败尝试与最终尝试的用量都必须保留且各计
    /// 一次，因此这里使用累加而非替换。cache 明细按已上报的部分求和；两侧都未知
    /// 时保持未知（不以 0 冒充未知）。token 计数按饱和加法，不使调用失败。
    pub fn accumulate(total: &mut Option<Self>, extra: Self) {
        match total {
            Some(total) => {
                total.input_tokens = total.input_tokens.saturating_add(extra.input_tokens);
                total.output_tokens = total.output_tokens.saturating_add(extra.output_tokens);
                total.cache_creation_input_tokens = sum_optional_tokens(
                    total.cache_creation_input_tokens,
                    extra.cache_creation_input_tokens,
                );
                total.cache_read_input_tokens = sum_optional_tokens(
                    total.cache_read_input_tokens,
                    extra.cache_read_input_tokens,
                );
            }
            None => *total = Some(extra),
        }
    }
}

fn sum_optional_tokens(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(
            left.unwrap_or_default()
                .saturating_add(right.unwrap_or_default()),
        ),
    }
}

/// provider 返回的完成停止原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other { value: String },
}

/// 模型公开能力，用于上层请求投影。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_streaming: bool,
}

/// 内建 provider 协议类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAiCompatible,
    OpenAiResponses,
    Anthropic,
    Other { value: String },
}
