//! # custom_middleware
//!
//! 演示如何实现自定义中间件（AgentState 扩展）

use async_trait::async_trait;
use peri_agent::prelude::*;

struct ContextInjectorMiddleware {
    key: String,
    value: String,
}

impl ContextInjectorMiddleware {
    fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[async_trait]
impl Middleware<AgentState> for ContextInjectorMiddleware {
    fn name(&self) -> &str {
        "context-injector"
    }

    async fn before_agent(&self, state: &mut AgentState) -> AgentResult<()> {
        state.set_context(&self.key, &self.value);
        println!("[context-injector] Injected: {} = {}", self.key, self.value);
        Ok(())
    }

    async fn after_agent(
        &self,
        state: &mut AgentState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        let suffix = state
            .get_context(&self.key)
            .map(|v| format!("\n[Context: {} = {}]", self.key, v))
            .unwrap_or_default();
        Ok(AgentOutput {
            text: format!("{}{}", output.text, suffix),
            steps: output.steps,
            tool_calls: output.tool_calls.clone(),
            stop_reason: output.stop_reason.clone(),
            block_continue: output.block_continue.clone(),
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent = ReActAgent::new(MockLLM::always_answer("这是回答内容"))
        .add_middleware(Box::new(LoggingMiddleware::new()))
        .add_middleware(Box::new(ContextInjectorMiddleware::new(
            "user_id", "user_42",
        )));

    let mut state = AgentState::new("/workspace");
    let output = agent
        .execute(AgentInput::text("你好"), &mut state, None)
        .await?;

    println!("\n=== Output with Context ===");
    println!("{}", output.text);
    Ok(())
}
