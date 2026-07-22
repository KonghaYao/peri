use std::time::Duration;

/// Langfuse Client 认证配置
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub public_key: String,
    pub secret_key: String,
    pub base_url: String,
    /// Turn 级采样率 0.0~1.0，默认 1.0（全报）
    pub trace_sampling: f64,
    /// 错误 turn 强制发 ErrorSpan 挂同 turn
    pub error_span_always: bool,
    /// Batcher 单批次最大事件数
    pub batch_max_events: usize,
    /// Batcher flush 间隔秒数
    pub batch_flush_interval_secs: u64,
    /// Batcher 背压策略
    pub batch_backpressure: BackpressurePolicy,
}

impl ClientConfig {
    /// 从环境变量构造配置
    /// 读取 LANGFUSE_PUBLIC_KEY、LANGFUSE_SECRET_KEY、LANGFUSE_BASE_URL
    /// base_url 默认值为 "https://cloud.langfuse.com"
    pub fn from_env() -> Result<Self, crate::LangfuseError> {
        let public_key = std::env::var("LANGFUSE_PUBLIC_KEY")
            .map_err(|_| crate::LangfuseError::Config("LANGFUSE_PUBLIC_KEY not set".into()))?;
        let secret_key = std::env::var("LANGFUSE_SECRET_KEY")
            .map_err(|_| crate::LangfuseError::Config("LANGFUSE_SECRET_KEY not set".into()))?;
        let base_url = std::env::var("LANGFUSE_BASE_URL")
            .unwrap_or_else(|_| "https://cloud.langfuse.com".to_string());
        Ok(Self {
            public_key,
            secret_key,
            base_url,
            trace_sampling: 1.0,
            error_span_always: true,
            batch_max_events: 50,
            batch_flush_interval_secs: 10,
            batch_backpressure: BackpressurePolicy::default(),
        })
    }
}

/// 背压策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackpressurePolicy {
    /// 队列满时丢弃新事件
    #[default]
    DropNew,
    /// 队列满时阻塞等待
    Block,
    /// 队列满时弹出最旧事件
    DropOldest,
}

/// Batcher 批量聚合配置
#[derive(Debug, Clone)]
pub struct BatcherConfig {
    pub max_events: usize,
    pub flush_interval: Duration,
    pub backpressure: BackpressurePolicy,
    pub max_retries: usize,
}

impl Default for BatcherConfig {
    fn default() -> Self {
        Self {
            max_events: 50,
            flush_interval: Duration::from_secs(10),
            backpressure: BackpressurePolicy::default(),
            max_retries: 3,
        }
    }
}

impl BatcherConfig {
    /// 从 ClientConfig 构造 Batcher 配置
    pub fn from_client(client: &ClientConfig) -> Self {
        Self {
            max_events: client.batch_max_events,
            flush_interval: Duration::from_secs(client.batch_flush_interval_secs),
            backpressure: client.batch_backpressure,
            max_retries: 3,
        }
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
