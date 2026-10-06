use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::error::{AgentError, AgentResult};
use crate::messages::BaseMessage;
use crate::tools::BaseTool;
use std::sync::Arc;

pub(crate) struct PreparedFixtureLlm {
    inner: Box<dyn ReactLLM + Send + Sync>,
    tools: Vec<Arc<dyn BaseTool>>,
}

impl PreparedFixtureLlm {
    pub(crate) fn new(
        inner: Box<dyn ReactLLM + Send + Sync>,
        tools: Vec<Arc<dyn BaseTool>>,
    ) -> Self {
        Self { inner, tools }
    }
}

#[async_trait::async_trait]
impl ReactLLM for PreparedFixtureLlm {
    fn prepare_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> AgentResult<peri_model::PreparedModelCall> {
        let checkpoint = serde_json::json!({
            "provider": "explicit-fixture",
            "model": self.model_name(),
            "endpoint": "fixture://prepared-reasoning",
            "credentialRef": "fixture:no-credentials",
            "body": {
                "messages": messages,
                "tools": tools.iter().map(|tool| serde_json::json!({
                    "name": tool.name(), "description": tool.description(), "parameters": tool.parameters(),
                })).collect::<Vec<_>>(),
            },
        });
        Ok(peri_model::PreparedModelCall::new(checkpoint, |_| {
            Ok(peri_model::ModelStream::new(futures::stream::empty()))
        }))
    }

    async fn generate_prepared_reasoning(
        &self,
        prepared: peri_model::PreparedModelCall,
        streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let messages: Vec<BaseMessage> =
            serde_json::from_value(prepared.checkpoint()["body"]["messages"].clone())
                .map_err(|error| AgentError::LlmError(error.to_string()))?;
        let names = prepared.checkpoint()["body"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let tools = names
            .iter()
            .map(|name| {
                self.tools
                    .iter()
                    .find(|tool| tool.name() == name)
                    .expect("prepared fixture tool must be explicitly supplied")
                    .as_ref()
            })
            .collect::<Vec<_>>();
        let cancellation = streaming
            .as_ref()
            .map(|streaming| streaming.cancel.clone())
            .unwrap_or_default();
        prepared
            .start(cancellation)
            .map_err(AgentError::ModelError)?;
        self.inner
            .generate_reasoning(&messages, &tools, streaming)
            .await
    }

    async fn generate_reasoning(
        &self,
        _: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        panic!("durable fixture must consume its prepared request")
    }

    fn model_name(&self) -> String {
        "fixture-scripted".into()
    }
    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        self.inner.provider_capabilities()
    }
}
