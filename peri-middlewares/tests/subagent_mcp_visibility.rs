use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::{
    agent::{react::ToolCall, stages::SharedToolMap},
    middleware::{capabilities::CatalogState, r#trait::Middleware},
    session::{
        subagent::{build_v2_subagent_context, SubagentChainAssembler, SubagentChainContext},
        tool_catalog::{ToolFilterPolicy, ToolSource},
        FrozenContext, Session,
    },
    tools::{BaseTool, ToolContext},
};
use peri_middlewares::subagent::{SubAgentMiddleware, SubagentChainAssemblerImpl};
use tokio_util::sync::CancellationToken;

struct LookupTool;

#[async_trait]
impl BaseTool for LookupTool {
    fn name(&self) -> &str {
        "mcp__late__lookup"
    }
    fn description(&self) -> &str {
        "Lookup a late-connected capability"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn is_direct(&self) -> bool {
        false
    }
    fn mcp_server_name(&self) -> Option<&str> {
        Some("late")
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        Some("lookup")
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("late capability executed".to_string())
    }
}

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
            ModelResponse::new(
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

#[derive(Clone)]
struct LookupLLM(Arc<std::sync::atomic::AtomicUsize>, bool);

impl LookupLLM {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::inline_fixture::*;
        let _ = &cancellation;
        let messages = base_messages(&request);
        let defined = defined_tools(&request);
        let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

        assert!(!tools.iter().any(|tool| tool.name() == "mcp__late__lookup"));
        assert!(tools.iter().any(|tool| tool.name() == "SearchExtraTools"));
        assert!(tools.iter().any(|tool| tool.name() == "ExecuteExtraTool"));
        let step = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if step == 1 && !self.1 {
            assert!(!messages
                .last()
                .unwrap()
                .content()
                .contains("mcp__late__lookup"));
            return text_events("verified dynamic tool revocation");
        }
        match step {
            0 => tool_events_from_react(vec![ToolCall::new(
                "search",
                "SearchExtraTools",
                serde_json::json!({"query": "select:mcp__late__lookup"}),
            )]),
            1 => {
                assert!(messages
                    .last()
                    .unwrap()
                    .content()
                    .contains("mcp__late__lookup"));
                tool_events_from_react(vec![ToolCall::new(
                    "execute",
                    "ExecuteExtraTool",
                    serde_json::json!({"tool_name": "mcp__late__lookup", "params": {}}),
                )])
            }
            _ => {
                assert!(messages
                    .last()
                    .unwrap()
                    .content()
                    .contains("late capability executed"));
                text_events("verified deferred MCP execution")
            }
        }
    }
}
inline_fixture::fixture_model_impl!(LookupLLM);

struct CatalogProbe(SharedToolMap, Option<ToolSource>);

impl CatalogState for CatalogProbe {
    fn local_tools(&self) -> Option<&SharedToolMap> {
        Some(&self.0)
    }
    fn push_recall(&mut self, _item: String) {}
    fn tool_source(&self, name: &str) -> Option<ToolSource> {
        (name == "mcp__late__lookup")
            .then(|| self.1.clone())
            .flatten()
    }
}

#[tokio::test]
async fn fork_inherits_late_static_mcp_tools_and_discovers_then_executes_them() {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let middleware = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_| {
            inline_fixture::fixture_source(std::sync::Arc::new(LookupLLM(
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                true,
            )))
        }),
    );
    middleware.set_parent_session(parent);
    let tools: BTreeMap<String, Arc<dyn BaseTool>> = middleware
        .collect_tools("/tmp")
        .into_iter()
        .map(|tool| (tool.name().to_string(), Arc::from(tool)))
        .collect();
    let mut probe = CatalogProbe(Arc::new(RwLock::new(tools)), None);
    probe
        .0
        .write()
        .insert("mcp__late__lookup".to_string(), Arc::new(LookupTool));
    middleware.before_reason_catalog(&mut probe).await.unwrap();
    let agent = probe.0.read()["Agent"].clone();
    let result = agent
        .invoke(
            serde_json::json!({"fork": true, "prompt": "use the MCP lookup"}),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    assert!(
        result.contains("verified deferred MCP execution"),
        "{result}"
    );
}

fn filtered_child(
    allowed: Option<Vec<String>>,
    disallowed: Vec<String>,
) -> peri_agent::session::subagent::V2SubagentContext {
    let chain = SubagentChainAssemblerImpl::new().assemble(&SubagentChainContext {
        cwd: "/tmp".to_string(),
        skill_names: vec![],
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        meta_harness_disabled: Default::default(),
    });
    build_v2_subagent_context(
        None,
        Box::new(peri_agent::agent::model_bridge::AgentModelBridge::new(
            std::sync::Arc::new(LookupLLM(
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                true,
            )),
        )),
        Arc::new(chain),
        vec![Arc::new(LookupTool)],
        ToolFilterPolicy::canonical(allowed, disallowed),
        None,
        "/tmp",
        CancellationToken::new(),
        None,
        None,
        None,
        None,
        None,
    )
}

#[test]
fn explicit_zero_tools_does_not_gain_search_or_execution_tools() {
    let child = filtered_child(Some(vec![]), vec![]);
    assert!(child
        .context
        .runtime
        .tool_catalog
        .snapshot()
        .tools
        .is_empty());
}

#[test]
fn denied_mcp_tools_are_absent_from_the_child_search_and_execution_catalog() {
    let child = filtered_child(None, vec!["mcp__late__lookup".to_string()]);
    let snapshot = child.context.runtime.tool_catalog.snapshot();
    assert!(snapshot.tools.contains_key("SearchExtraTools"));
    assert!(snapshot.tools.contains_key("ExecuteExtraTool"));
    assert!(!snapshot.tools.contains_key("mcp__late__lookup"));
}

#[tokio::test]
async fn unloaded_dynamic_tools_are_not_frozen_into_static_child_inheritance() {
    use peri_acp_types::dynamic_mcp::{
        DynamicMcpIncarnationId, DynamicMcpInstanceKey, DynamicMcpLogicalKey,
    };

    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let middleware = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_| {
            inline_fixture::fixture_source(std::sync::Arc::new(LookupLLM(
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                false,
            )))
        }),
    );
    middleware.set_parent_session(parent);
    let tools = middleware
        .collect_tools("/tmp")
        .into_iter()
        .map(|tool| (tool.name().to_string(), Arc::from(tool)))
        .collect();
    let source = ToolSource::DynamicMcp(DynamicMcpInstanceKey {
        logical: DynamicMcpLogicalKey {
            session_id: "parent".to_string(),
            server_name: "late".to_string(),
        },
        incarnation_id: DynamicMcpIncarnationId::from_string("revoked".to_string()),
    });
    let mut probe = CatalogProbe(Arc::new(RwLock::new(tools)), Some(source));
    probe
        .0
        .write()
        .insert("mcp__late__lookup".to_string(), Arc::new(LookupTool));
    middleware.before_reason_catalog(&mut probe).await.unwrap();
    let agent = probe.0.read()["Agent"].clone();
    let result = agent
        .invoke(
            serde_json::json!({"fork": true, "prompt": "check revoked tools"}),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    assert!(
        result.contains("verified dynamic tool revocation"),
        "{result}"
    );
}
