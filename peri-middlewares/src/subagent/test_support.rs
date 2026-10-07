//! 测试夹具：`peri_model::Model` 形态的假模型助手（middlewares 本地副本，
//! 与 peri-agent test resources 同语义；跨 crate 无法复用 cfg(test) 模块）。
//!
//! H1 起子链统一消费模型来源（`SubagentLlmSource::Model`）并由生产装配点自建
//! bridge（身份 + 请求时贡献 + 流式事件）。测试的假模型因此直接实现
//! [`peri_model::Model`]：`prepare_stream`（durable checkpoint）+ `stream`
//! （Started 语义由真实事件流表达：`TextDelta` → `Completed`），并保留取消语义。
use futures::StreamExt;
use peri_agent::agent::react::ToolCall as ReactToolCall;
use peri_agent::messages::{BaseMessage, ToolCallRequest};
use peri_agent::tools::BaseTool;
use peri_model::{
    JsonObject, Model, ModelRequest, ModelResponse, ModelResult, ModelStream, ModelStreamEvent,
    StopReason,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 请求 → 会话消息（假模型据此断言/回显）。
pub(crate) fn base_messages(request: &ModelRequest) -> Vec<BaseMessage> {
    request
        .messages
        .iter()
        .map(|message| match message {
            peri_model::ModelMessage::System { content } => BaseMessage::system(text_of(content)),
            peri_model::ModelMessage::User { content } => BaseMessage::human(text_of(content)),
            peri_model::ModelMessage::Assistant {
                content,
                tool_calls,
            } => BaseMessage::ai_with_tool_calls(
                text_of(content),
                tool_calls
                    .iter()
                    .map(|call| {
                        ToolCallRequest::new(
                            call.id().to_string(),
                            call.name().to_string(),
                            serde_json::Value::Object(
                                call.arguments().as_map().clone().into_iter().collect(),
                            ),
                        )
                    })
                    .collect(),
            ),
            peri_model::ModelMessage::ToolResult { result } => {
                let text = text_of(&result.content);
                if result.is_error {
                    BaseMessage::tool_error(result.tool_call_id.clone(), text)
                } else {
                    BaseMessage::tool_result(result.tool_call_id.clone(), text)
                }
            }
        })
        .collect()
}

/// 由请求工具定义合成的只读工具：假模型看到与真实请求一致的工具名/描述/参数。
pub(crate) struct DefinedTool {
    pub(crate) name: String,
    description: String,
    parameters: serde_json::Value,
}

#[async_trait::async_trait]
impl BaseTool for DefinedTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> serde_json::Value {
        self.parameters.clone()
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err("fixture tool is not executable".into())
    }
}

pub(crate) fn defined_tools(request: &ModelRequest) -> Vec<DefinedTool> {
    request
        .tools
        .iter()
        .map(|definition| DefinedTool {
            name: definition.name.clone(),
            description: definition.description.clone().unwrap_or_default(),
            parameters: serde_json::Value::Object(
                definition
                    .input_schema
                    .as_map()
                    .clone()
                    .into_iter()
                    .collect(),
            ),
        })
        .collect()
}

pub(crate) fn tool_names(request: &ModelRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

fn text_of(content: &[peri_model::ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            peri_model::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// 文本事件：`TextDelta`（流式事件入口）→ `Completed`（同一 poll 序列）。
pub(crate) fn text_events(text: impl Into<String>) -> Vec<ModelResult<ModelStreamEvent>> {
    let text = text.into();
    let mut events: Vec<ModelResult<ModelStreamEvent>> = Vec::new();
    if !text.is_empty() {
        events.push(Ok(ModelStreamEvent::TextDelta { text: text.clone() }));
    }
    events.push(Ok(ModelStreamEvent::Completed(response_with_text(text))));
    events
}

/// 工具调用事件：`ToolCallDelta` → `Completed`（assistant tool_calls）。
pub(crate) fn tool_events(calls: Vec<ToolCallRequest>) -> Vec<ModelResult<ModelStreamEvent>> {
    let tool_calls = calls
        .into_iter()
        .map(|call| {
            peri_model::ToolCall::new(
                call.id,
                call.name,
                JsonObject::from_value(call.arguments).unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    let mut events: Vec<ModelResult<ModelStreamEvent>> = Vec::new();
    for (index, call) in tool_calls.iter().enumerate() {
        events.push(Ok(ModelStreamEvent::ToolCallDelta {
            index,
            id: Some(call.id().to_string()),
            name: Some(call.name().to_string()),
            arguments_delta: serde_json::to_string(call.arguments()).unwrap_or_default(),
        }));
    }
    events.push(Ok(ModelStreamEvent::Completed(
        ModelResponse::new(
            peri_model::ModelMessage::Assistant {
                content: Vec::new(),
                tool_calls,
            },
            StopReason::ToolUse,
            None,
            None,
        )
        .expect("fixture tool response"),
    )));
    events
}

/// 先发可见增量，再以错误终止（保留已发出的增量）。
/// React 层 ToolCall → 事件序列（旧假 LLM 直接构造 `Reasoning::with_tools`）。
pub(crate) fn tool_events_from_react(
    calls: Vec<ReactToolCall>,
) -> Vec<ModelResult<ModelStreamEvent>> {
    tool_events(
        calls
            .into_iter()
            .map(|call| ToolCallRequest::new(call.id, call.name, call.input))
            .collect(),
    )
}

pub(crate) fn error_events(
    mut events: Vec<ModelResult<ModelStreamEvent>>,
    error: peri_model::ModelError,
) -> Vec<ModelResult<ModelStreamEvent>> {
    events.push(Err(error));
    events
}

/// 旧假 LLM 的 `Reasoning` 结果 → 事件序列（保留文本与 usage）。
pub(crate) fn events_from_reasoning(
    reasoning: peri_agent::agent::react::Reasoning,
) -> Vec<ModelResult<ModelStreamEvent>> {
    let text = reasoning
        .final_answer
        .clone()
        .unwrap_or_else(|| reasoning.thought.clone());
    let mut events: Vec<ModelResult<ModelStreamEvent>> = Vec::new();
    if !text.is_empty() {
        events.push(Ok(ModelStreamEvent::TextDelta { text: text.clone() }));
    }
    if let Some(usage) = reasoning.usage.clone() {
        events.push(Ok(ModelStreamEvent::Usage(usage)));
    }
    if reasoning.tool_calls.is_empty() {
        events.push(Ok(ModelStreamEvent::Completed(response_with_text(text))));
    } else {
        events.push(Ok(ModelStreamEvent::Completed(
            ModelResponse::new(
                peri_model::ModelMessage::Assistant {
                    content: vec![peri_model::ContentBlock::text(text)],
                    tool_calls: reasoning
                        .tool_calls
                        .iter()
                        .map(|call| {
                            peri_model::ToolCall::new(
                                call.id.clone(),
                                call.name.clone(),
                                JsonObject::from_value(call.input.clone()).unwrap_or_default(),
                            )
                        })
                        .collect(),
                },
                StopReason::ToolUse,
                reasoning.usage.clone(),
                reasoning.request_id.clone(),
            )
            .expect("fixture reasoning response"),
        )));
    }
    events
}

pub(crate) fn lead_chunk(text: &str) -> ModelResult<ModelStreamEvent> {
    Ok(ModelStreamEvent::TextDelta {
        text: text.to_string(),
    })
}

pub(crate) fn completed_event(text: impl Into<String>) -> ModelResult<ModelStreamEvent> {
    let text = text.into();
    Ok(ModelStreamEvent::Completed(response_with_text(text)))
}

pub(crate) fn response_with_text(text: impl Into<String>) -> ModelResponse {
    ModelResponse::new(
        peri_model::ModelMessage::assistant_text(text),
        StopReason::EndTurn,
        None,
        None,
    )
    .expect("fixture text response")
}

/// 为假模型生成 `Model` 实现：`respond` 持有全部测试行为（可 await 门控）并返回
/// 事件序列；`prepare_stream` 冻结请求、在 start 时惰性执行（取消可中断）。
///
/// 本 crate 测试模块本地宏（`pub(crate) use` 暴露为 `crate::subagent::test_support::fixture_model_impl!`），
/// 不作为生产 API 导出。
macro_rules! fixture_model_impl {
    ($ty:ty) => {
        #[async_trait::async_trait]
        impl peri_model::Model for $ty {
            fn capabilities(&self) -> peri_model::ModelCapabilities {
                peri_model::ModelCapabilities {
                    supports_streaming: true,
                    supports_tools: true,
                    ..peri_model::ModelCapabilities::default()
                }
            }

            fn prepare_stream(
                &self,
                request: peri_model::ModelRequest,
            ) -> peri_model::ModelResult<peri_model::PreparedModelCall> {
                let this = self.clone();
                let checkpoint = serde_json::json!({
                    "provider": "fixture-model",
                    "model": "fixture-scripted",
                    "endpoint": "https://fixture.invalid/messages",
                    "credentialRef": "fixture:no-credentials",
                    "body": &request,
                });
                Ok(peri_model::PreparedModelCall::new(
                    checkpoint,
                    move |cancellation| {
                        let this = this.clone();
                        let token = cancellation.clone();
                        let stream = futures::StreamExt::flatten(futures::stream::once(
                            async move {
                                futures::stream::iter(this.respond(request, cancellation).await)
                            },
                        ));
                        Ok(peri_model::ModelStream::with_parent_cancellation(stream, token))
                    },
                ))
            }

            async fn stream(
                &self,
                request: peri_model::ModelRequest,
                cancellation: tokio_util::sync::CancellationToken,
            ) -> peri_model::ModelResult<peri_model::ModelStream> {
                let events = self.respond(request, cancellation.clone()).await;
                Ok(peri_model::ModelStream::with_parent_cancellation(
                    futures::stream::iter(events),
                    cancellation,
                ))
            }
        }
    };
}

/// 子模型来源：测试假模型 → 统一装配入口（生产 bridge）。
pub(crate) fn fixture_source(
    model: Arc<dyn Model>,
    name: &str,
) -> peri_agent::session::subagent::SubagentLlmSource {
    peri_agent::session::subagent::SubagentLlmSource::model(model, name)
}
pub(crate) use fixture_model_impl;
