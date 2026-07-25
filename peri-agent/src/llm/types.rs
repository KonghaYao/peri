use tokio_util::sync::CancellationToken;

use crate::{messages::BaseMessage, tools::ToolDefinition};

/// LLM 请求
pub struct LlmRequest {
    pub messages: Vec<BaseMessage>,
    pub tools: Vec<ToolDefinition>,
    /// Anthropic system 字段（OpenAI 通过 System 消息传递）
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// 会话级 ID，用于 LiteLLM 等代理按 session 聚合多次 LLM 请求
    pub session_id: Option<String>,
}

impl LlmRequest {
    pub fn new(messages: Vec<BaseMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            system: None,
            max_tokens: None,
            temperature: None,
            session_id: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Token 使用量（adapter 层规范化后的统一语义，所有 provider 一致）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    /// 总输入 token（含缓存 token，adapter 层已规范化）
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 写入缓存的 token 数（仅 Anthropic 有意义，OpenAI 始终 None）
    pub cache_creation_input_tokens: Option<u32>,
    /// 从缓存读取的 token 数（Anthropic/OpenAI 均有，某些模型为 None）
    pub cache_read_input_tokens: Option<u32>,
    /// API 提供商返回的请求 ID
    pub request_id: Option<String>,
    /// 首 token 时间（RFC3339 格式，流式调用时记录）
    /// 非流式调用时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_time: Option<String>,
}

/// LLM 响应
pub struct LlmResponse {
    /// Ai 变体消息
    pub message: BaseMessage,
    pub stop_reason: StopReason,
    /// Token 使用量（可选，不支持的 LLM 为 None）
    pub usage: Option<TokenUsage>,
    /// API 提供商返回的请求 ID
    pub request_id: Option<String>,
}

/// 停止原因
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

impl StopReason {
    pub fn from_display(s: &str) -> Self {
        match s {
            "end_turn" => StopReason::EndTurn,
            "tool_use" => StopReason::ToolUse,
            "max_tokens" => StopReason::MaxTokens,
            other => StopReason::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopReason::EndTurn => write!(f, "end_turn"),
            StopReason::ToolUse => write!(f, "tool_use"),
            StopReason::MaxTokens => write!(f, "max_tokens"),
            StopReason::Other(s) => write!(f, "{}", s),
        }
    }
}

use std::sync::Arc;

use crate::{agent::events::AgentEventHandler, messages::MessageId};

/// 流式输出上下文，由 Executor 注入到 LLM 适配器。
/// LLM 适配器在 SSE 解析过程中通过 event_handler 发射增量事件。
pub struct StreamingContext {
    pub event_handler: Arc<dyn AgentEventHandler>,
    /// 预生成的 AI 消息 ID，所有增量 TextChunk 关联到此 ID
    pub message_id: MessageId,
    /// 取消令牌：LLM 适配器在流式循环中检查，触发时中断请求
    pub cancel: CancellationToken,
}

impl Clone for StreamingContext {
    fn clone(&self) -> Self {
        Self {
            event_handler: Arc::clone(&self.event_handler),
            message_id: self.message_id,
            cancel: self.cancel.clone(),
        }
    }
}

impl StopReason {
    pub fn from_openai(s: &str) -> Self {
        match s {
            "stop" => Self::EndTurn,
            "tool_calls" => Self::ToolUse,
            "length" => Self::MaxTokens,
            other => Self::Other(other.to_string()),
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
