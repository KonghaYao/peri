// [TRAP] Reasoning 序列化 / Provider 特定处理
// - 不要把 Reasoning 序列化为 {"type":"thinking"} 发给不支持的 provider（DeepSeek 报 unknown variant）
// - 过滤 Reasoning 时必须同时作为顶层 reasoning_content 字段回传
// - messages_to_json 中 reasoning 字段已移除，仅回传 reasoning_content
// - stream_options.include_usage 仅 Qwen 发送
// - Kimi：thinking_enabled 时移除 reasoning_effort
// 详见 spec/global/domains/agent.md#issue_2026-05-12-thinking-reasoning-dataflow-issues
use async_trait::async_trait;

use super::super::BaseModel;
use crate::{
    error::AgentResult,
    llm::provider_adapter::{GenericInvoker, ProviderAdapter},
    llm::types::{LlmRequest, LlmResponse, StreamingContext},
};

#[async_trait]
impl BaseModel for super::ChatOpenAI {
    async fn invoke(&self, request: LlmRequest) -> AgentResult<LlmResponse> {
        GenericInvoker::invoke(&self.adapter, &self.client, request).await
    }

    fn provider_name(&self) -> &str {
        "openai"
    }

    fn model_id(&self) -> &str {
        self.adapter.model_id()
    }

    fn context_window(&self) -> u32 {
        self.context_window_inner()
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

    /// Langfuse Generation input：返回 OpenAI Provider-native 请求体
    fn build_request_body(&self, request: &LlmRequest) -> Option<serde_json::Value> {
        Some(self.adapter.build_request_body(request, false))
    }
}
