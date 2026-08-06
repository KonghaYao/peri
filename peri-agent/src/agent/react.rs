use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use peri_acp_types::identity::AgentId;
use peri_model::{StopReason, TokenUsage};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::events_v2::EventBus,
    messages::{BaseMessage, MessageContent},
    session::turn::TurnId,
    tools::BaseTool,
};

/// Agent 输入
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInput {
    /// 输入内容（支持纯文字或多模态 MessageContent）
    pub content: MessageContent,
    /// 附加参数
    pub params: HashMap<String, serde_json::Value>,
}

impl AgentInput {
    /// 纯文本输入（最常见场景）
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: MessageContent::text(text.into()),
            params: HashMap::new(),
        }
    }

    /// 多模态输入（图片 + 文字等）
    pub fn blocks(content: MessageContent) -> Self {
        Self {
            content,
            params: HashMap::new(),
        }
    }

    pub fn with_param(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }
}

/// Agent 输出
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub text: String,
    pub steps: usize,
    pub tool_calls: Vec<(ToolCall, ToolResult)>,
    /// Agent 停止原因。传给 Stop hook 的 source 字段。
    /// 例如 "agent_complete"（正常结束）、"max_iterations"（达到上限）等。
    /// 目前仅正常完成时为 None，未来可扩展更多 reason。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Stop hook block 继续原因。Some 表示 hook 要求 agent 继续工作。
    /// 由 HookMiddleware::after_agent 设置，executor 检测后跳过 Done 继续循环。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_continue: Option<String>,
}

impl AgentOutput {
    pub fn new(text: impl Into<String>, steps: usize) -> Self {
        Self {
            text: text.into(),
            steps,
            tool_calls: Vec::new(),
            stop_reason: None,
            block_continue: None,
        }
    }
}

/// 工具调用请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input,
        }
    }
}

/// 工具调用结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub tool_name: String,
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            output: message.into(),
            is_error: true,
        }
    }
}

/// LLM 推理结果（ReAct 单步）
#[derive(Debug, Clone)]
pub struct Reasoning {
    pub thought: String,
    pub tool_calls: Vec<ToolCall>,
    pub final_answer: Option<String>,
    /// 原始 LLM 响应消息（含 Reasoning/Text blocks），优先用于存 state
    pub source_message: Option<BaseMessage>,
    /// Token 使用量（来自 LLM 响应，用于 Langfuse Generation 追踪）
    pub usage: Option<TokenUsage>,
    /// API 提供商返回的请求 ID。
    pub request_id: Option<String>,
    /// 生成此推理的模型名称
    pub model: String,
    /// 标记是否已通过事件流式发射过文本（由流式 LLM 适配器设为 true）
    pub streamed: bool,
    /// LLM 响应的停止原因（end_turn / tool_use / max_tokens）
    pub stop_reason: StopReason,
}

impl Reasoning {
    pub fn with_tools(thought: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            thought: thought.into(),
            tool_calls,
            final_answer: None,
            source_message: None,
            usage: None,
            request_id: None,
            model: String::new(),
            streamed: false,
            stop_reason: StopReason::ToolUse,
        }
    }

    pub fn with_answer(thought: impl Into<String>, answer: impl Into<String>) -> Self {
        Self {
            thought: thought.into(),
            tool_calls: Vec::new(),
            final_answer: Some(answer.into()),
            source_message: None,
            usage: None,
            request_id: None,
            model: String::new(),
            streamed: false,
            stop_reason: StopReason::EndTurn,
        }
    }

    pub fn needs_tool_call(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// 流式输出上下文，由 Reason 阶段注入到 ReactLLM。
///
/// 承载 v2 事件总线与身份（turn_id / agent_id）：LLM 适配器在流式解析过程中
/// 直接 emit v2 `RenderEvent`（TextChunk / ThinkingChunk）与 `ObserveEvent`
/// （AiReasoningChunk）。v1 `ExecutorEvent` 流式中间态已退役（v1 兼容映射仅
/// 保留在 ACP 协议序列化面，`peri-acp-types::event_v2::*_event_to_executor`）。
#[derive(Clone)]
pub struct StreamingContext {
    /// v2 事件总线（发射点：流式增量直接 emit RenderEvent/ObserveEvent）
    pub event_bus: Arc<EventBus>,
    /// 当前 turn（v2 事件强制身份字段）
    pub turn_id: TurnId,
    /// 当前 agent（v2 事件强制身份字段）
    pub agent_id: AgentId,
    /// 取消令牌：bridge 将其传入底层 Model stream。
    pub cancel: CancellationToken,
}

/// ReAct LLM trait
#[async_trait::async_trait]
pub trait ReactLLM: Send + Sync {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning>;

    /// 返回当前模型名称（用于 Langfuse Generation 追踪）
    fn model_name(&self) -> String {
        "unknown".to_string()
    }

    /// 返回模型的上下文窗口大小（token 数），默认 200K
    fn context_window(&self) -> u32 {
        200_000
    }

    /// 返回由安全 `PreparedModelRequest` 投影出的 Provider 请求体，用于受控观测。
    ///
    /// 此方法绝不返回 headers 或认证信息。默认实现返回 None。
    fn observed_provider_request_body(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
    ) -> Option<serde_json::Value> {
        None
    }

    /// 生成推理，并同时返回该次 LLM 调用实际请求体的受控观测值。
    ///
    /// 默认实现 = `generate_reasoning` + `observed_provider_request_body`（两次独立
    /// 构建 request）。生产实现（`AgentModelBridge`）应覆盖本方法，让观测复用
    /// `generate_reasoning` 内部已构建的同一份 request，消除每轮 LLM 调用的
    /// 双构建。
    async fn generate_reasoning_with_observed_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<(Reasoning, Option<serde_json::Value>)> {
        let reasoning = self.generate_reasoning(messages, tools, streaming).await?;
        let body = self.observed_provider_request_body(messages, tools);
        Ok((reasoning, body))
    }

    /// 构造 Provider 实际请求体（raw body）。
    fn build_provider_request_body(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
    ) -> Option<serde_json::Value> {
        None
    }

    /// 返回 Provider 能力（消息协议类型、签名 reasoning 处理规则）。
    /// 默认返回 Generic 安全保守值。
    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

/// Blanket impl：允许将 Box<dyn ReactLLM + Send + Sync> 直接用于 v2 stages
#[async_trait::async_trait]
impl ReactLLM for Box<dyn ReactLLM + Send + Sync> {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        (**self)
            .generate_reasoning(messages, tools, streaming)
            .await
    }

    fn model_name(&self) -> String {
        (**self).model_name()
    }

    fn context_window(&self) -> u32 {
        (**self).context_window()
    }

    fn observed_provider_request_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> Option<serde_json::Value> {
        (**self).observed_provider_request_body(messages, tools)
    }

    async fn generate_reasoning_with_observed_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<(Reasoning, Option<serde_json::Value>)> {
        (**self)
            .generate_reasoning_with_observed_body(messages, tools, streaming)
            .await
    }

    fn build_provider_request_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> Option<serde_json::Value> {
        (**self).build_provider_request_body(messages, tools)
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        (**self).provider_capabilities()
    }
}
