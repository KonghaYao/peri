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
    /// 遥测 spool、内容编码与内存预算；本入口不读取新环境键，只填默认值。
    pub telemetry: TelemetryConfig,
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
            telemetry: TelemetryConfig::default(),
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

/// 上报正文的内容编码；`Gzip` 是标准 OTLP/HTTP 压缩，`None` 保留未压缩格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    /// 标准 gzip。
    #[default]
    Gzip,
    /// 不压缩。
    None,
}

/// 遥测 spool、内容编码与内存预算（契约 §6）。
///
/// 配置层（`peri-config`）只逐键解析来源；同一份配置面的跨键约束由
/// [`TelemetryConfig::validate`] 在构造期一次校验，失败返回 [`crate::LangfuseError::Config`]。
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// 磁盘暂存开关；false 时只使用有界内存路径。
    pub spool_enabled: bool,
    /// 上报正文内容编码。
    pub compression: Compression,
    /// 超过该字节数的冻结 input 进入暂存。
    pub input_spool_threshold_bytes: usize,
    /// 超过该字节数的导出正文进入暂存文件。
    pub payload_memory_threshold_bytes: usize,
    /// retained、工作区与缓冲的总预算字节数。
    pub memory_budget_bytes: usize,
    /// 同时准备的编码任务数。
    pub prepare_max_in_flight: usize,
    /// 编码读写块字节数。
    pub stream_chunk_bytes: usize,
    /// 暂存磁盘配额字节数。
    pub spool_quota_bytes: u64,
    /// 暂存对象存活时间；按 owner 的单调时钟计时，必须非零。
    pub spool_ttl: Duration,
    /// 压缩后 wire 正文字节上限；必须非零。
    pub max_wire_bytes: usize,
    /// 关闭时等待遥测移交的宽限时间；必须非零。
    pub shutdown_grace: Duration,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            spool_enabled: true,
            compression: Compression::Gzip,
            input_spool_threshold_bytes: 256 * 1024,
            payload_memory_threshold_bytes: 256 * 1024,
            memory_budget_bytes: 32 * 1024 * 1024,
            prepare_max_in_flight: 1,
            stream_chunk_bytes: 64 * 1024,
            spool_quota_bytes: 1024 * 1024 * 1024,
            spool_ttl: Duration::from_secs(86_400),
            max_wire_bytes: 4 * 1024 * 1024,
            shutdown_grace: Duration::from_secs(60),
        }
    }
}

impl TelemetryConfig {
    /// 校验遥测配置；只有通过校验的配置才能进入预算预留与运行时。
    ///
    /// 规则：并发准备槽非零且不超 `Semaphore` 范围；字节预算非零；阈值与读写块不超总内存
    /// 预算；时限非零且与当前单调时钟相加不溢出。不放大配置值，也不向上取整。
    pub fn validate(&self) -> Result<(), crate::LangfuseError> {
        if self.prepare_max_in_flight == 0
            || self.prepare_max_in_flight > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(crate::LangfuseError::Config(
                "telemetry prepare capacity is outside the supported nonzero range".into(),
            ));
        }
        if self.memory_budget_bytes == 0
            || self.stream_chunk_bytes == 0
            || self.spool_quota_bytes == 0
            || self.max_wire_bytes == 0
        {
            return Err(crate::LangfuseError::Config(
                "telemetry byte budgets must be nonzero".into(),
            ));
        }
        if self.input_spool_threshold_bytes > self.memory_budget_bytes
            || self.payload_memory_threshold_bytes > self.memory_budget_bytes
            || self.stream_chunk_bytes > self.memory_budget_bytes
        {
            return Err(crate::LangfuseError::Config(
                "telemetry thresholds and chunk size must fit the memory budget".into(),
            ));
        }
        if self.spool_ttl.is_zero()
            || self.shutdown_grace.is_zero()
            || peri_time::monotonic_now()
                .checked_add(self.spool_ttl)
                .is_none()
            || peri_time::monotonic_now()
                .checked_add(self.shutdown_grace)
                .is_none()
        {
            return Err(crate::LangfuseError::Config(
                "invalid telemetry spool lifetime or shutdown grace".into(),
            ));
        }
        Ok(())
    }
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
    /// 遥测 spool、内容编码与内存预算。
    pub telemetry: TelemetryConfig,
}

impl Default for BatcherConfig {
    fn default() -> Self {
        Self {
            max_events: 50,
            queue_capacity: 1024,
            max_in_flight: 2,
            max_event_bytes: 4 * 1024 * 1024,
            max_batch_bytes: 4 * 1024 * 1024,
            max_queue_bytes: 16 * 1024 * 1024,
            flush_interval: Duration::from_secs(10),
            backpressure: BackpressurePolicy::default(),
            telemetry: TelemetryConfig::default(),
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
        self.telemetry.validate()?;
        // 契约 §3.1/§4：配置层必须证明记账总额能隔离进度分区 P，只有 `limit - P` 的 retained
        // 分区可用于 source/event/payload；不能把「预算装不下 P」拖到运行期构造才发现。
        //
        // 复用部署构造的同一份 checked 乘加与 `limit > P` 判断（`memory::for_batcher`），不维护
        // 第二份公式；响应预留按 `ExportConfig` 冻结默认值先做必要检查，调用方传入更大的
        // export 预算时由构造期以实际值复核。此处只判定，不做实际隔离——隔离是构造期的事。
        crate::memory::MemoryBudget::for_batcher(self, &crate::client::ExportConfig::default())
            .map(|_| ())?;
        Ok(())
    }

    /// 从 ClientConfig 构造 Batcher 配置；遥测配置逐字段复制。
    pub fn from_client(client: &ClientConfig) -> Self {
        Self {
            max_events: client.batch_max_events,
            flush_interval: Duration::from_secs(client.batch_flush_interval_secs),
            backpressure: client.batch_backpressure,
            telemetry: client.telemetry.clone(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
