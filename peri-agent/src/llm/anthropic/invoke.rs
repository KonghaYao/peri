use async_trait::async_trait;

use super::super::BaseModel;
use crate::{
    error::AgentResult,
    llm::provider_adapter::{GenericInvoker, ProviderAdapter},
    llm::types::{LlmRequest, LlmResponse, StreamingContext},
};

#[async_trait]
impl BaseModel for super::ChatAnthropic {
    async fn invoke(&self, request: LlmRequest) -> AgentResult<LlmResponse> {
        GenericInvoker::invoke(&self.adapter, &self.client, request).await
    }

    fn provider_name(&self) -> &str {
        "anthropic"
    }

    fn model_id(&self) -> &str {
        self.adapter.model_id()
    }

    fn context_window(&self) -> u32 {
        self.adapter.context_window()
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn invoke_streaming(
        &self,
        request: LlmRequest,
        ctx: StreamingContext,
    ) -> AgentResult<LlmResponse> {
        super::stream::do_invoke_streaming(&self.adapter, &self.client, request, ctx).await
    }

    fn build_request_body(&self, request: &LlmRequest) -> Option<serde_json::Value> {
        Some(self.adapter.build_request_body(request, false))
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::anthropic()
    }
}
