//! Tests for retry

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::*;
use crate::error::AgentError;

/// Mock LLM：按脚本返回成功或失败
struct MockLLM {
    results: Arc<Vec<AgentResult<Reasoning>>>,
    call_count: AtomicUsize,
}

impl MockLLM {
    fn new(results: Vec<AgentResult<Reasoning>>) -> Self {
        Self {
            results: Arc::new(results),
            call_count: AtomicUsize::new(0),
        }
    }

    fn _call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
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
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if idx < self.results.len() {
            match &self.results[idx] {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(clone_error(e)),
            }
        } else {
            Err(AgentError::LlmError("unexpected call".into()))
        }
    }

    fn model_name(&self) -> String {
        "mock".to_string()
    }

    fn context_window(&self) -> u32 {
        200_000
    }
}

fn clone_error(e: &AgentError) -> AgentError {
    match e {
        AgentError::LlmError(msg) => AgentError::LlmError(msg.clone()),
        AgentError::LlmHttpError { status, message } => AgentError::LlmHttpError {
            status: *status,
            message: message.clone(),
        },
        AgentError::ToolNotFound(name) => AgentError::ToolNotFound(name.clone()),
        _ => AgentError::LlmError(e.to_string()),
    }
}

fn ok_reasoning() -> AgentResult<Reasoning> {
    Ok(Reasoning::with_answer("", "test response"))
}

fn http_error(status: u16) -> AgentResult<Reasoning> {
    Err(AgentError::LlmHttpError {
        status,
        message: format!("API 错误 {}", status),
    })
}

fn network_error(msg: &str) -> AgentResult<Reasoning> {
    Err(AgentError::LlmError(msg.to_string()))
}

/// 前两次 503，第三次成功 → 最终 Ok
#[tokio::test]
async fn test_retry_then_success() {
    let mock = MockLLM::new(vec![http_error(503), http_error(503), ok_reasoning()]);
    let retry = RetryableLLM::new(mock, RetryConfig::default().with_base_delay_ms(1));
    let result = retry.generate_reasoning(&[], &[], None).await;
    assert!(result.is_ok());
}

/// 400 错误立即返回，不重试
#[tokio::test]
async fn test_non_retryable_immediate_return() {
    let mock = MockLLM::new(vec![http_error(400)]);
    let retry = RetryableLLM::new(mock, RetryConfig::default().with_base_delay_ms(1));
    let result = retry.generate_reasoning(&[], &[], None).await;
    assert!(result.is_err());
    if let Err(AgentError::LlmHttpError { status, .. }) = result {
        assert_eq!(status, 400);
    } else {
        panic!("Expected LlmHttpError(400)");
    }
}

/// 重试耗尽，返回最后一次错误
#[tokio::test]
async fn test_retry_exhausted() {
    let mock = MockLLM::new(vec![http_error(429), http_error(429), http_error(429)]);
    let config = RetryConfig::default()
        .with_max_retries(2)
        .with_base_delay_ms(1);
    let retry = RetryableLLM::new(mock, config);
    let result = retry.generate_reasoning(&[], &[], None).await;
    assert!(result.is_err());
    if let Err(AgentError::LlmHttpError { status, .. }) = result {
        assert_eq!(status, 429);
    } else {
        panic!("Expected LlmHttpError(429)");
    }
}

/// 网络错误可重试
#[tokio::test]
async fn test_network_error_retryable() {
    let mock = MockLLM::new(vec![network_error("connection refused"), ok_reasoning()]);
    let retry = RetryableLLM::new(mock, RetryConfig::default().with_base_delay_ms(1));
    let result = retry.generate_reasoning(&[], &[], None).await;
    assert!(result.is_ok());
}

/// 退避延迟范围验证
#[test]
fn test_exponential_delay_range() {
    let config = RetryConfig::default();
    for attempt in 0..=5 {
        let delay = config.exponential_delay(attempt);
        let effective = attempt + 1;
        let base = (config.base_delay_ms as f64 * 2f64.powi(effective as i32))
            .min(config.max_delay_ms as f64);
        let lower = base as u64;
        let upper = (base * 1.25) as u64;
        assert!(
            delay >= lower && delay <= upper,
            "attempt {}: delay {} not in [{}, {}]",
            attempt,
            delay,
            lower,
            upper,
        );
    }
}

/// 验证最终尝试（重试耗尽后）直接返回结果，不进入重试逻辑
#[tokio::test]
async fn test_final_attempt_no_retry() {
    // 重试耗尽后的最终尝试：可重试错误也直接返回
    let mock = MockLLM::new(vec![http_error(429), http_error(429), http_error(429)]);
    let config = RetryConfig::default()
        .with_max_retries(2)
        .with_base_delay_ms(1);
    let retry = RetryableLLM::new(mock, config);
    let result = retry.generate_reasoning(&[], &[], None).await;
    assert!(result.is_err());
    // 重试 2 次（attempt 0,1）+ 最终尝试（返回错误）= 共 3 次调用
    // 脚本只有 3 个错误，恰好覆盖
    if let Err(AgentError::LlmHttpError { status, .. }) = result {
        assert_eq!(status, 429, "最终尝试应返回最后一次错误");
    } else {
        panic!("Expected LlmHttpError(429)");
    }
}

/// 验证 max_retries=0 时只执行一次调用（无重试）
#[tokio::test]
async fn test_zero_retries_single_attempt() {
    let mock = MockLLM::new(vec![ok_reasoning()]);
    let config = RetryConfig::default().with_max_retries(0);
    let retry = RetryableLLM::new(mock, config);
    let result = retry.generate_reasoning(&[], &[], None).await;
    assert!(result.is_ok(), "max_retries=0 时应直接返回结果");
}

/// 验证取消令牌可中断重试循环
#[tokio::test]
async fn test_cancel_token_interrupts_retry() {
    let mock = MockLLM::new(vec![http_error(503), http_error(503), ok_reasoning()]);
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let config = RetryConfig::default().with_base_delay_ms(1);
    let retry = RetryableLLM::new(mock, config).with_cancel_token(token);
    let result = retry.generate_reasoning(&[], &[], None).await;
    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "已取消的 token 应立即使 retry 返回 Interrupted"
    );
}
