use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::stages::SharedToolMap;
use peri_agent::middleware::capabilities::CatalogState;
use peri_agent::session::subagent;
use peri_agent::session::subagent::SubagentChainContext;
use peri_agent::session::tool_catalog::ToolFilterPolicy;
use peri_agent::session::tool_catalog::ToolSource;
use peri_agent::session::FrozenContext;
use peri_agent::session::Session;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ToolContext;
use peri_middlewares::subagent::{SubAgentMiddleware, SubagentChainAssemblerImpl};
use tokio_util::sync::CancellationToken;

// ─── 集成测试本地 Model 夹具（不跨 crate 共享 test_helpers） ──────────────
mod inline_fixture {
    use peri_agent::messages::{BaseMessage, ToolCallRequest};
    use peri_agent::tools::BaseTool;
    use peri_model::{JsonObject, ModelResponse, ModelResult, ModelStreamEvent, StopReason};
    use std::sync::Arc;

    pub(super) fn base_messages(request: &peri_model::ModelRequest) -> Vec<BaseMessage> {
        request
            .messages
            .iter()
            .map(|message| match message {
                peri_model::ModelMessage::System { content } => {
                    BaseMessage::system(text_of(content))
                }
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

    fn text_of(content: &[peri_model::ContentBlock]) -> String {
        content
            .iter()
            .filter_map(|block| match block {
                peri_model::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub(super) struct DefinedTool {
        pub(super) name: String,
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

    pub(super) fn defined_tools(request: &peri_model::ModelRequest) -> Vec<DefinedTool> {
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

    pub(super) fn text_events(text: impl Into<String>) -> Vec<ModelResult<ModelStreamEvent>> {
        let text = text.into();
        let mut events: Vec<ModelResult<ModelStreamEvent>> = Vec::new();
        if !text.is_empty() {
            events.push(Ok(ModelStreamEvent::TextDelta { text: text.clone() }));
        }
        events.push(Ok(ModelStreamEvent::Completed(
            ModelResponse::try_new(
                peri_model::ModelMessage::assistant_text(text),
                StopReason::EndTurn,
                None,
                None,
            )
            .expect("fixture response"),
        )));
        events
    }

    pub(super) fn tool_events_from_react(
        calls: Vec<peri_agent::agent::react::ToolCall>,
    ) -> Vec<ModelResult<ModelStreamEvent>> {
        let tool_calls = calls
            .into_iter()
            .map(|call| {
                peri_model::ToolCall::new(
                    call.id,
                    call.name,
                    JsonObject::from_value(call.input).unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let mut events: Vec<ModelResult<ModelStreamEvent>> = Vec::new();
        {
            let indexed_tool_calls = tool_calls.iter().enumerate();
            for (index, call) in indexed_tool_calls {
                events.push(Ok(ModelStreamEvent::ToolCallDelta {
                    index,
                    id: Some(call.id().to_string()),
                    name: Some(call.name().to_string()),
                    arguments_delta: serde_json::to_string(call.arguments()).unwrap_or_default(),
                }));
            }
        }
        events.push(Ok(ModelStreamEvent::Completed(
            ModelResponse::try_new(
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

    macro_rules! fixture_model_impl_local {
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
                        "provider": "inline-fixture",
                        "model": "fixture-scripted",
                        "endpoint": "https://fixture.invalid/messages",
                        "credentialRef": "fixture:no-credentials",
                        "body": &request,
                    });
                    Ok(peri_model::PreparedModelCall::new(checkpoint, move |cancellation| {
                        let this = this.clone();
                        let token = cancellation.clone();
                        let stream = futures::StreamExt::flatten(futures::stream::once(async move {
                            futures::stream::iter(this.respond(request, cancellation).await)
                        }));
                        Ok(peri_model::ModelStream::with_parent_cancellation(stream, token))
                    }))
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
    pub(super) use fixture_model_impl_local as fixture_model_impl;
    pub(super) fn fixture_source(
        model: Arc<dyn peri_model::Model>,
    ) -> peri_agent::session::subagent::SubagentLlmSource {
        peri_agent::session::subagent::SubagentLlmSource::model(model, "fixture-scripted")
    }
}

#[path = "subagent_mcp_visibility/test_cases.rs"]
mod test_cases;
