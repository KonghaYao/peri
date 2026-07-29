use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;

use crate::{
    agent::react::{ReactLLM, Reasoning, ToolCall},
    error::AgentResult,
    messages::BaseMessage,
    tools::BaseTool,
};

/// Mock ReactLLM - 用于测试，按预设脚本返回推理结果
///
/// `script` 创建后不再修改，用 `Arc<Vec<_>>` 共享；
/// `index` 用原子计数器替代 Mutex，消除在 async fn 中持有同步锁的潜在风险。
pub struct MockLLM {
    script: Arc<Vec<Reasoning>>,
    index: Arc<AtomicUsize>,
}

impl MockLLM {
    pub fn new(script: Vec<Reasoning>) -> Self {
        Self {
            script: Arc::new(script),
            index: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn always_answer(answer: impl Into<String>) -> Self {
        let answer = answer.into();
        Self::new(vec![Reasoning::with_answer("Thinking...", answer)])
    }

    pub fn tool_then_answer(
        tool_name: impl Into<String>,
        tool_input: serde_json::Value,
        answer: impl Into<String>,
    ) -> Self {
        let call = ToolCall::new("call_1", tool_name, tool_input);
        Self::new(vec![
            Reasoning::with_tools("I need to use a tool", vec![call]),
            Reasoning::with_answer("Based on the tool result", answer),
        ])
    }
}

#[async_trait]
impl ReactLLM for MockLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<crate::llm::types::StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let idx = self.index.fetch_add(1, Ordering::Relaxed);
        let reasoning = self
            .script
            .get(idx)
            .or_else(|| self.script.last())
            .cloned()
            .unwrap_or_else(|| Reasoning::with_answer("(no more script)", "Done"));
        Ok(reasoning)
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

#[cfg(test)]
#[path = "adapter_test.rs"]
mod tests;
