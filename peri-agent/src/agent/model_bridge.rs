use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use peri_model::{
    ContentBlock as ModelContentBlock, DocumentSource as ModelDocumentSource,
    ImageSource as ModelImageSource, JsonObject, Model, ModelMessage, ModelRequest, ModelResponse,
    ModelStreamEvent, ToolCall as ModelToolCall, ToolDefinition as ModelToolDefinition,
    ToolResult as ModelToolResult,
};

use crate::{
    agent::{
        compact_v2::projection::{ProviderCapabilities, ProviderProtocol},
        events::ExecutorEvent,
        react::{ReactLLM, Reasoning, StreamingContext, ToolCall},
    },
    error::{AgentError, AgentResult},
    messages::{
        BaseMessage, ContentBlock, DocumentSource, ImageSource, MessageContent, ToolCallRequest,
    },
    tools::BaseTool,
};

/// 将标准 `peri_model::Model` 接入 ReAct 的 Agent 边界。
pub struct AgentModelBridge {
    model: Arc<dyn Model>,
    system: Option<String>,
    session_id: Option<String>,
}

impl AgentModelBridge {
    pub fn new(model: Arc<dyn Model>) -> Self {
        Self {
            model,
            system: None,
            session_id: None,
        }
    }

    pub fn from_arc(model: Arc<dyn Model>) -> Self {
        Self::new(model)
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// 测试与过渡期调用方使用的单条消息转换入口。
    #[cfg(test)]
    pub(crate) fn convert_message(message: &BaseMessage) -> AgentResult<ModelMessage> {
        Self::convert_messages(std::slice::from_ref(message)).map(|mut messages| messages.remove(0))
    }

    fn convert_messages(messages: &[BaseMessage]) -> AgentResult<Vec<ModelMessage>> {
        let mut tool_names = BTreeMap::new();
        for message in messages {
            if let BaseMessage::Ai { tool_calls, .. } = message {
                for call in tool_calls {
                    tool_names.insert(call.id.clone(), call.name.clone());
                }
            }
        }

        messages
            .iter()
            .map(|message| match message {
                BaseMessage::System { content, .. } => Ok(ModelMessage::System {
                    content: convert_content(content)?,
                }),
                BaseMessage::Human { content, .. } => Ok(ModelMessage::User {
                    content: convert_content(content)?,
                }),
                BaseMessage::Ai {
                    content,
                    tool_calls,
                    ..
                } => Ok(ModelMessage::Assistant {
                    content: convert_content(content)?,
                    tool_calls: tool_calls
                        .iter()
                        .map(convert_tool_call)
                        .collect::<AgentResult<_>>()?,
                }),
                BaseMessage::Tool {
                    tool_call_id,
                    content,
                    is_error,
                    ..
                } => {
                    let name = tool_names.get(tool_call_id).ok_or_else(|| {
                        AgentError::LlmError("tool result has no matching tool name".into())
                    })?;
                    Ok(ModelMessage::ToolResult {
                        result: ModelToolResult {
                            id: None,
                            tool_call_id: tool_call_id.clone(),
                            name: name.clone(),
                            content: convert_content(content)?,
                            is_error: *is_error,
                        },
                    })
                }
            })
            .collect()
    }

    fn build_request(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> AgentResult<ModelRequest> {
        let mut messages = Self::convert_messages(messages)?;
        if let Some(system) = &self.system {
            messages.insert(0, ModelMessage::system_text(system));
        }
        let tools = tools
            .iter()
            .map(|tool| {
                Ok(ModelToolDefinition::new(
                    tool.name(),
                    JsonObject::from_value(tool.parameters()).map_err(map_model_error)?,
                )
                .with_description(tool.description()))
            })
            .collect::<AgentResult<_>>()?;
        Ok(ModelRequest {
            messages,
            tools,
            session_id: self.session_id.clone(),
            ..Default::default()
        })
    }

    fn response_reasoning(response: ModelResponse, streamed: bool) -> AgentResult<Reasoning> {
        let usage = response.usage().cloned();
        let request_id = response.request_id().map(str::to_owned);
        let stop_reason = response.stop_reason().clone();
        let source_message = convert_model_message(response.message())?;
        let (content, tool_calls) = match response.message() {
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => (content, tool_calls),
            _ => {
                return Err(AgentError::LlmError(
                    "model response must be assistant".into(),
                ))
            }
        };
        let thought: String = content
            .iter()
            .filter_map(|block| match block {
                ModelContentBlock::Reasoning { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let answer = content
            .iter()
            .filter_map(ModelContentBlock::text_content)
            .collect::<String>();
        let tool_calls = tool_calls
            .iter()
            .map(|call| {
                Ok(ToolCall::new(
                    call.id(),
                    call.name(),
                    serde_json::Value::Object(
                        call.arguments().as_map().clone().into_iter().collect(),
                    ),
                ))
            })
            .collect::<AgentResult<Vec<_>>>()?;
        let has_answer = !answer.is_empty();
        let mut reasoning = if tool_calls.is_empty() {
            Reasoning::with_answer(thought, answer.clone())
        } else {
            Reasoning::with_tools(thought, tool_calls)
        };
        // Assistant 文本和 tool calls 可以在同一条完成消息中共存；不能因 ReAct
        // 当前要进入 Act 阶段就静默丢弃已生成文本。
        if has_answer {
            reasoning.final_answer = Some(answer);
        }
        reasoning.source_message = Some(source_message);
        reasoning.usage = usage;
        reasoning.request_id = request_id;
        reasoning.stop_reason = stop_reason;
        reasoning.streamed = streamed;
        Ok(reasoning)
    }

    /// 消费已构建的 request，驱动 Model 流式响应直至完成。
    ///
    /// 从 `generate_reasoning` 提取（2026-08 重构）：让观测路径
    /// （`generate_reasoning_with_observed_body`）与普通路径共享同一份
    /// 已构建的 request，消除每轮 LLM 调用的 request 双构建。
    async fn generate_from_request(
        &self,
        request: ModelRequest,
        streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let model_name = self.model_name();
        let cancellation = streaming
            .as_ref()
            .map(|context| context.cancel.clone())
            .unwrap_or_default();
        let mut stream = self
            .model
            .stream(request, cancellation.clone())
            .await
            .map_err(map_model_error)?;

        loop {
            let event = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    stream.abort();
                    return Err(AgentError::Interrupted);
                }
                event = stream.next() => event,
            };
            match event {
                Some(Ok(ModelStreamEvent::TextDelta { text })) => {
                    if let Some(context) = &streaming {
                        if context.cancel.is_cancelled() {
                            stream.abort();
                            return Err(AgentError::Interrupted);
                        }
                        context.event_handler.on_event(ExecutorEvent::TextChunk {
                            message_id: context.message_id,
                            chunk: text,
                            source_agent_id: None,
                        });
                    }
                }
                Some(Ok(ModelStreamEvent::ReasoningDelta { text })) => {
                    if let Some(context) = &streaming {
                        if context.cancel.is_cancelled() {
                            stream.abort();
                            return Err(AgentError::Interrupted);
                        }
                        context.event_handler.on_event(ExecutorEvent::AiReasoning {
                            text,
                            source_agent_id: None,
                        });
                    }
                }
                Some(Ok(ModelStreamEvent::ToolCallDelta { .. } | ModelStreamEvent::Usage(_))) => {}
                Some(Ok(ModelStreamEvent::Completed(response))) => {
                    let mut reasoning = Self::response_reasoning(response, streaming.is_some())?;
                    reasoning.model = model_name;
                    return Ok(reasoning);
                }
                Some(Err(error)) => return Err(map_model_error(error)),
                None => {
                    return Err(AgentError::LlmError(
                        "model stream ended without completion".into(),
                    ))
                }
            }
        }
    }
}

#[async_trait]
impl ReactLLM for AgentModelBridge {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let request = self.build_request(messages, tools)?;
        self.generate_from_request(request, streaming).await
    }

    async fn generate_reasoning_with_observed_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> AgentResult<(Reasoning, Option<serde_json::Value>)> {
        // 覆盖默认实现：只构建一次 request，观测体复用同一份（消除每轮双构建）
        let request = self.build_request(messages, tools)?;
        let observed_body = self
            .model
            .prepare_request(&request)
            .ok()
            .map(|prepared| prepared.body().as_value().clone());
        let reasoning = self.generate_from_request(request, streaming).await?;
        Ok((reasoning, observed_body))
    }

    fn model_name(&self) -> String {
        self.model
            .prepare_request(&ModelRequest::default())
            .map(|request| request.model_id().to_owned())
            .unwrap_or_else(|_| "unknown".into())
    }

    fn observed_provider_request_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> Option<serde_json::Value> {
        self.build_request(messages, tools)
            .ok()
            .and_then(|request| self.model.prepare_request(&request).ok())
            .map(|request| request.body().as_value().clone())
    }

    fn provider_capabilities(&self) -> ProviderCapabilities {
        // 协议身份来自 prepared request 观测投影；prepare_request 失败时保守回退 Generic。
        // Anthropic 带签名 reasoning 必须整体保留（compact 投影依赖此判定），不能对
        // 所有 provider 一律报告 Generic——否则 Anthropic 的 signed reasoning 会被
        // projection 当成可截断内容处理。
        let protocol = self
            .model
            .prepare_request(&ModelRequest::default())
            .map(|request| match request.protocol() {
                peri_model::ProviderProtocol::OpenAiCompatible => ProviderProtocol::OpenAI,
                peri_model::ProviderProtocol::Anthropic => ProviderProtocol::Anthropic,
                peri_model::ProviderProtocol::Other { .. } => ProviderProtocol::Generic,
            })
            .unwrap_or(ProviderProtocol::Generic);
        ProviderCapabilities {
            signed_reasoning_must_be_whole: protocol == ProviderProtocol::Anthropic,
            protocol,
        }
    }
}

fn convert_tool_call(call: &ToolCallRequest) -> AgentResult<ModelToolCall> {
    Ok(ModelToolCall::new(
        call.id.clone(),
        call.name.clone(),
        JsonObject::from_value(call.arguments.clone()).map_err(map_model_error)?,
    ))
}

fn convert_content(content: &MessageContent) -> AgentResult<Vec<ModelContentBlock>> {
    match content {
        MessageContent::Text(text) => Ok(vec![ModelContentBlock::text(text)]),
        MessageContent::Raw(_) => Err(AgentError::LlmError(
            "unsupported agent content: raw provider content".into(),
        )),
        MessageContent::Blocks(blocks) => blocks.iter().map(convert_block).collect(),
    }
}

fn convert_block(block: &ContentBlock) -> AgentResult<ModelContentBlock> {
    match block {
        ContentBlock::Text { text } => Ok(ModelContentBlock::text(text)),
        ContentBlock::Image { source } => Ok(ModelContentBlock::Image {
            source: match source {
                ImageSource::Base64 { media_type, data } => ModelImageSource::Base64 {
                    media_type: peri_model::MediaType::new(media_type),
                    data: data.clone(),
                },
                ImageSource::Url { url } => ModelImageSource::Url { url: url.clone() },
            },
        }),
        ContentBlock::Document { source, title } => Ok(ModelContentBlock::Document {
            source: match source {
                DocumentSource::Base64 { media_type, data } => ModelDocumentSource::Base64 {
                    media_type: peri_model::MediaType::new(media_type),
                    data: data.clone(),
                },
                DocumentSource::Url { url } => ModelDocumentSource::Url { url: url.clone() },
                DocumentSource::Text { text } => ModelDocumentSource::Text { text: text.clone() },
            },
            title: title.clone(),
        }),
        ContentBlock::ToolUse { id, name, input } => Ok(ModelContentBlock::ToolUse {
            tool_call: ModelToolCall::new(
                id.clone(),
                name.clone(),
                JsonObject::from_value(input.clone()).map_err(map_model_error)?,
            ),
        }),
        ContentBlock::ToolResult {
            id,
            tool_use_id,
            content,
            is_error,
        } => Ok(ModelContentBlock::ToolResult {
            result: Box::new(ModelToolResult {
                id: id.clone(),
                tool_call_id: tool_use_id.clone(),
                name: "unknown".into(),
                content: content
                    .iter()
                    .map(convert_block)
                    .collect::<AgentResult<_>>()?,
                is_error: *is_error,
            }),
        }),
        ContentBlock::Reasoning { text, signature } => Ok(ModelContentBlock::Reasoning {
            text: text.clone(),
            signature: signature.clone(),
        }),
        ContentBlock::Unknown(value) => {
            // Agent 暂无显式 RedactedReasoning 变体；标准载荷经 Unknown 容器往返。
            // 仅识别 peri_model::RedactedReasoning 的确定形状，其余 Unknown 一律 fail closed。
            if value.get("type").and_then(|t| t.as_str()) == Some("redacted_reasoning") {
                let data = value
                    .get("data")
                    .and_then(|d| d.as_str())
                    .map(str::to_owned);
                return Ok(ModelContentBlock::RedactedReasoning { data });
            }
            Err(AgentError::LlmError(
                "unsupported agent content: unknown content block".into(),
            ))
        }
    }
}

fn convert_model_message(message: &ModelMessage) -> AgentResult<BaseMessage> {
    match message {
        ModelMessage::System { content } => Ok(BaseMessage::system(MessageContent::Blocks(
            content
                .iter()
                .map(convert_model_block)
                .collect::<AgentResult<_>>()?,
        ))),
        ModelMessage::User { content } => Ok(BaseMessage::human(MessageContent::Blocks(
            content
                .iter()
                .map(convert_model_block)
                .collect::<AgentResult<_>>()?,
        ))),
        ModelMessage::Assistant {
            content,
            tool_calls,
        } => Ok(BaseMessage::ai_with_tool_calls(
            MessageContent::Blocks(
                content
                    .iter()
                    .map(convert_model_block)
                    .collect::<AgentResult<_>>()?,
            ),
            tool_calls
                .iter()
                .map(|call| {
                    ToolCallRequest::new(
                        call.id(),
                        call.name(),
                        serde_json::Value::Object(
                            call.arguments().as_map().clone().into_iter().collect(),
                        ),
                    )
                })
                .collect(),
        )),
        ModelMessage::ToolResult { result } => Ok(if result.is_error {
            BaseMessage::tool_error(
                &result.tool_call_id,
                MessageContent::Blocks(
                    result
                        .content
                        .iter()
                        .map(convert_model_block)
                        .collect::<AgentResult<_>>()?,
                ),
            )
        } else {
            BaseMessage::tool_result(
                &result.tool_call_id,
                MessageContent::Blocks(
                    result
                        .content
                        .iter()
                        .map(convert_model_block)
                        .collect::<AgentResult<_>>()?,
                ),
            )
        }),
    }
}

fn convert_model_block(block: &ModelContentBlock) -> AgentResult<ContentBlock> {
    match block {
        ModelContentBlock::Text { text } => Ok(ContentBlock::text(text)),
        ModelContentBlock::Image { source } => Ok(ContentBlock::Image {
            source: match source {
                ModelImageSource::Base64 { media_type, data } => ImageSource::Base64 {
                    media_type: media_type.as_str().into(),
                    data: data.clone(),
                },
                ModelImageSource::Url { url } => ImageSource::Url { url: url.clone() },
            },
        }),
        ModelContentBlock::Document { source, title } => Ok(ContentBlock::Document {
            source: match source {
                ModelDocumentSource::Base64 { media_type, data } => DocumentSource::Base64 {
                    media_type: media_type.as_str().into(),
                    data: data.clone(),
                },
                ModelDocumentSource::Url { url } => DocumentSource::Url { url: url.clone() },
                ModelDocumentSource::Text { text } => DocumentSource::Text { text: text.clone() },
            },
            title: title.clone(),
        }),
        ModelContentBlock::Reasoning { text, signature } => Ok(ContentBlock::Reasoning {
            text: text.clone(),
            signature: signature.clone(),
        }),
        ModelContentBlock::ToolUse { tool_call } => Ok(ContentBlock::ToolUse {
            id: tool_call.id().into(),
            name: tool_call.name().into(),
            input: serde_json::Value::Object(
                tool_call.arguments().as_map().clone().into_iter().collect(),
            ),
        }),
        ModelContentBlock::ToolResult { result } => Ok(ContentBlock::ToolResult {
            id: result.id.clone(),
            tool_use_id: result.tool_call_id.clone(),
            content: result
                .content
                .iter()
                .map(convert_model_block)
                .collect::<AgentResult<_>>()?,
            is_error: result.is_error,
        }),
        ModelContentBlock::RedactedReasoning { data } => {
            Ok(ContentBlock::Unknown(serde_json::json!({
                "type": "redacted_reasoning",
                "data": data,
            })))
        }
    }
}

pub(crate) fn map_model_error(error: peri_model::ModelError) -> AgentError {
    if error.is_cancelled() {
        AgentError::Interrupted
    } else if let Some(status) = error.http_status_code() {
        AgentError::LlmHttpError {
            status,
            message: error.to_string(),
        }
    } else {
        AgentError::LlmError(error.to_string())
    }
}

/// legacy v1 wire format 的 stop_reason 字符串（JSON 字段值，非 Debug 形式）。
///
/// LlmCallEnd output JSON 的 `stop_reason` 字段依赖它，不能退化为 `{:?}` 的变体名。
pub(crate) fn stop_reason_display(reason: &peri_model::StopReason) -> String {
    match reason {
        peri_model::StopReason::EndTurn => "end_turn".into(),
        peri_model::StopReason::ToolUse => "tool_use".into(),
        peri_model::StopReason::MaxTokens => "max_tokens".into(),
        peri_model::StopReason::Other { value } => value.clone(),
    }
}

#[cfg(test)]
#[path = "model_bridge_test.rs"]
mod tests;
