use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use rand::RngExt;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{
        events::{AgentEventHandler, ExecutorEvent},
        react::{ReactLLM, Reasoning},
    },
    error::{AgentError, AgentResult},
    messages::BaseMessage,
    tools::BaseTool,
};

/// 重试配置
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: usize,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay_ms: 500,
            max_delay_ms: 32_000,
        }
    }
}

impl RetryConfig {
    pub fn with_max_retries(mut self, n: usize) -> Self {
        self.max_retries = n;
        self
    }
    pub fn with_base_delay_ms(mut self, ms: u64) -> Self {
        self.base_delay_ms = ms;
        self
    }
    pub fn with_max_delay_ms(mut self, ms: u64) -> Self {
        self.max_delay_ms = ms;
        self
    }

    /// 指数退避 + 25% 随机抖动
    ///
    /// attempt 从 0 开始，但首次重试（attempt=0）使用 base_delay * 2
    /// 以确保对 429 限流有足够等待时间。
    pub fn exponential_delay(&self, attempt: usize) -> u64 {
        let effective = attempt + 1;
        let base =
            (self.base_delay_ms as f64 * 2f64.powi(effective as i32)).min(self.max_delay_ms as f64);
        let mut rng = rand::rng();
        let jitter = rng.random_range(0.0..0.25) * base;
        (base + jitter) as u64
    }
}

/// ReactLLM 装饰器：在调用失败时自动重试
pub struct RetryableLLM<L: ReactLLM> {
    inner: L,
    config: RetryConfig,
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// 用于在重试循环中检查用户取消信号
    cancel_token: Option<Arc<CancellationToken>>,
}

impl<L: ReactLLM> RetryableLLM<L> {
    pub fn new(inner: L, config: RetryConfig) -> Self {
        Self {
            inner,
            config,
            event_handler: None,
            cancel_token: None,
        }
    }

    pub fn with_event_handler(mut self, handler: Arc<dyn AgentEventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    /// 设置 cancel token，用于在重试循环中感知用户取消信号
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(Arc::new(token));
        self
    }

    fn emit(&self, event: ExecutorEvent) {
        if let Some(h) = &self.event_handler {
            h.on_event(event);
        }
    }
}

#[async_trait]
impl<L: ReactLLM> ReactLLM for RetryableLLM<L> {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<crate::llm::types::StreamingContext>,
    ) -> AgentResult<Reasoning> {
        // 重试循环：attempt 0..max_retries，每次失败若可重试则延迟后继续
        for attempt in 0..self.config.max_retries {
            // 检查取消信号（用户按 Esc 中断时提前退出重试）
            if self
                .cancel_token
                .as_ref()
                .map(|c| c.is_cancelled())
                .unwrap_or(false)
            {
                return Err(AgentError::Interrupted);
            }
            // 仅首次尝试透传 streaming，重试时传 None 防止同一 message_id 双重发射
            let retry_streaming = if attempt == 0 {
                streaming.clone()
            } else {
                None
            };
            match self
                .inner
                .generate_reasoning(messages, tools, retry_streaming)
                .await
            {
                Ok(r) => return Ok(r),
                Err(e) if e.is_retryable() => {
                    let delay = self.config.exponential_delay(attempt);
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_retries = self.config.max_retries,
                        delay_ms = delay,
                        error = %e,
                        "LLM 调用失败，准备重试"
                    );
                    self.emit(ExecutorEvent::LlmRetrying {
                        attempt: attempt + 1,
                        max_attempts: self.config.max_retries,
                        delay_ms: delay,
                        error: e.to_string(),
                    });
                    crate::metrics::emit(
                        "llm.retry",
                        serde_json::json!({
                            "attempt": attempt + 1,
                            "max_attempts": self.config.max_retries,
                            "model": self.inner.model_name(),
                            "error": e.to_string(),
                            "delay_ms": delay,
                        }),
                        None,
                        None,
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Err(e) => return Err(e),
            }
        }
        // 最终尝试（不重试），直接返回结果或错误（重试已耗尽，传 None 避免双重发射）
        self.inner.generate_reasoning(messages, tools, None).await
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }

    fn context_window(&self) -> u32 {
        self.inner.context_window()
    }

    fn inject_event_handler(
        &mut self,
        handler: Option<std::sync::Arc<dyn crate::agent::events::AgentEventHandler>>,
    ) {
        self.event_handler = handler;
    }

    fn build_provider_request_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> Option<serde_json::Value> {
        self.inner.build_provider_request_body(messages, tools)
    }
}

#[cfg(test)]
#[path = "retry_test.rs"]
mod tests;
