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
    /// 队列满时替换最旧的待发送事件；已准入 flush 及其前缀不可驱逐。
    /// 无可驱逐事件时返回 QueueFull，在途 HTTP 批次不受影响。
    DropOldest,
}

/// Batcher 批量聚合配置
#[derive(Debug, Clone)]
pub struct BatcherConfig {
    /// 单批事件上限，必须为 1..=tokio::sync::Semaphore::MAX_PERMITS。
    pub max_events: usize,
    /// 独立命令槽数；Flush 与 Add 共用容量。
    pub queue_capacity: usize,
    /// 同时发送的批次数；flush 屏障会等待其前缀批次。
    pub max_in_flight: usize,
    /// 准入前计量的事件 JSON 字节上限，不截断事件。
    pub max_event_bytes: usize,
    /// 聚合输入及最终编码请求的字节上限。
    pub max_batch_bytes: usize,
    /// 已提交队列的事件 JSON 字节总量上限，不含调用方尚未提交的事件。
    pub max_queue_bytes: usize,
    /// 自动发送间隔，必须非零。
    pub flush_interval: Duration,
    pub backpressure: BackpressurePolicy,
    /// 兼容保留，不参与执行；实际重试次数由 LangfuseClient 构造参数独占。
    pub max_retries: usize,
}

impl Default for BatcherConfig {
    fn default() -> Self {
        Self {
            max_events: 50,
            queue_capacity: 1024,
            max_in_flight: 2,
            max_event_bytes: 512 * 1024,
            max_batch_bytes: 4 * 1024 * 1024,
            max_queue_bytes: 16 * 1024 * 1024,
            flush_interval: Duration::from_secs(10),
            backpressure: BackpressurePolicy::default(),
            max_retries: 3,
        }
    }
}

impl BatcherConfig {
    pub(crate) fn validate(&self) -> Result<(), crate::LangfuseError> {
        if [self.max_events, self.queue_capacity, self.max_in_flight]
            .into_iter()
            .any(|capacity| capacity == 0 || capacity > tokio::sync::Semaphore::MAX_PERMITS)
        {
            return Err(crate::LangfuseError::Config(
                "batch, queue or in-flight capacity is outside the supported nonzero range".into(),
            ));
        }
        if self.max_event_bytes == 0
            || self.max_event_bytes > self.max_batch_bytes
            || self.max_event_bytes > self.max_queue_bytes
        {
            return Err(crate::LangfuseError::Config(
                "event byte budget must be nonzero and fit batch and queue budgets".into(),
            ));
        }
        if self.flush_interval.is_zero() {
            return Err(crate::LangfuseError::Config(
                "batch flush_interval must be nonzero".into(),
            ));
        }
        Ok(())
    }

    /// 从 ClientConfig 构造 Batcher 配置
    pub fn from_client(client: &ClientConfig) -> Self {
        Self {
            max_events: client.batch_max_events,
            flush_interval: Duration::from_secs(client.batch_flush_interval_secs),
            backpressure: client.batch_backpressure,
            max_retries: 3,
            ..Default::default()
        }
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
