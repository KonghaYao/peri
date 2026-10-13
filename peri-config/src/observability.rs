use std::collections::BTreeMap;

use thiserror::Error;

#[path = "observability_spool.rs"]
mod spool;

pub const ENVIRONMENT_KEYS: &[&str] = &[
    "LANGFUSE_PUBLIC_KEY",
    "LANGFUSE_SECRET_KEY",
    "LANGFUSE_BASE_URL",
    "LANGFUSE_TRACE_SAMPLING",
    "LANGFUSE_ERROR_SPAN_ALWAYS",
    "LANGFUSE_BATCH_MAX_EVENTS",
    "LANGFUSE_BATCH_QUEUE_CAPACITY",
    "LANGFUSE_BATCH_MAX_IN_FLIGHT",
    "LANGFUSE_BATCH_MAX_EVENT_BYTES",
    "LANGFUSE_BATCH_MAX_BYTES",
    "LANGFUSE_BATCH_MAX_QUEUE_BYTES",
    "LANGFUSE_BATCH_FLUSH_INTERVAL",
    "LANGFUSE_USER_ID",
    spool::SPOOL_ENABLED.environment,
    spool::COMPRESSION.environment,
    spool::INPUT_SPOOL_THRESHOLD_BYTES.environment,
    spool::PAYLOAD_MEMORY_THRESHOLD_BYTES.environment,
    spool::MEMORY_BUDGET_BYTES.environment,
    spool::PREPARE_MAX_IN_FLIGHT.environment,
    spool::STREAM_CHUNK_BYTES.environment,
    spool::SPOOL_QUOTA_BYTES.environment,
    spool::SPOOL_TTL_SECS.environment,
    spool::MAX_WIRE_BYTES.environment,
    spool::SHUTDOWN_GRACE_SECS.environment,
];

/// 新配置键的胜出来源；旧键继续使用“忽略非法值”的兼容语义，不报告来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigValueSource {
    /// `langfuse.*` settings 键。
    Settings,
    /// 具名环境变量。
    Environment,
}

/// 新配置键的解析失败；只携带键名、来源与原因，不携带原配置值。
#[derive(Debug, Error)]
pub enum LangfuseConfigError {
    #[error("invalid langfuse value for {key} from {origin:?}: {reason}")]
    Invalid {
        key: &'static str,
        origin: ConfigValueSource,
        reason: &'static str,
    },
}

/// 上报正文的内容编码；`gzip` 为标准 OTLP/HTTP 压缩，`none` 保留未压缩格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LangfuseCompression {
    /// 标准 gzip。
    #[default]
    Gzip,
    /// 不压缩。
    None,
}

#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
    pub host: String,
    pub trace_sampling: f64,
    pub error_span_always: bool,
    pub batch_max_events: usize,
    pub batch_queue_capacity: usize,
    pub batch_max_in_flight: usize,
    pub batch_max_event_bytes: usize,
    pub batch_max_bytes: usize,
    pub batch_max_queue_bytes: usize,
    pub batch_flush_interval_secs: u64,
    pub user_id: Option<String>,
    /// 磁盘暂存开关；false 时只使用有界内存路径。
    pub spool_enabled: bool,
    /// 上报正文内容编码。
    pub compression: LangfuseCompression,
    /// 超过该字节数的冻结 input 进入暂存。
    pub input_spool_threshold_bytes: usize,
    /// 超过该字节数的导出正文进入暂存文件。
    pub payload_memory_threshold_bytes: usize,
    /// 遥测 retained、工作区与缓冲的总预算字节数。
    pub memory_budget_bytes: usize,
    /// 同时准备的编码任务数。
    pub prepare_max_in_flight: usize,
    /// 编码读写块字节数。
    pub stream_chunk_bytes: usize,
    /// 暂存磁盘配额字节数。
    pub spool_quota_bytes: u64,
    /// 暂存对象存活秒数。
    pub spool_ttl_secs: u64,
    /// 压缩后 wire 正文字节上限。
    pub max_wire_bytes: usize,
    /// 关闭时等待遥测移交的宽限秒数。
    pub shutdown_grace_secs: u64,
}

impl Default for LangfuseConfig {
    fn default() -> Self {
        Self {
            public_key: None,
            secret_key: None,
            host: "https://cloud.langfuse.com".to_string(),
            trace_sampling: 1.0,
            error_span_always: true,
            batch_max_events: 50,
            batch_queue_capacity: 1024,
            batch_max_in_flight: 2,
            batch_max_event_bytes: 4 * 1024 * 1024,
            batch_max_bytes: 4 * 1024 * 1024,
            batch_max_queue_bytes: 16 * 1024 * 1024,
            batch_flush_interval_secs: 10,
            user_id: None,
            spool_enabled: true,
            compression: LangfuseCompression::Gzip,
            input_spool_threshold_bytes: 256 * 1024,
            payload_memory_threshold_bytes: 256 * 1024,
            memory_budget_bytes: 32 * 1024 * 1024,
            prepare_max_in_flight: 1,
            stream_chunk_bytes: 64 * 1024,
            spool_quota_bytes: 1024 * 1024 * 1024,
            spool_ttl_secs: 86_400,
            max_wire_bytes: 4 * 1024 * 1024,
            shutdown_grace_secs: 60,
        }
    }
}

impl LangfuseConfig {
    pub fn from_env() -> Result<Option<Self>, LangfuseConfigError> {
        let config = Self::load_with_settings(&serde_json::Value::Null)?;
        Ok(
            Some(config)
                .filter(|config| config.public_key.is_some() && config.secret_key.is_some()),
        )
    }

    pub fn load_with_settings(settings: &serde_json::Value) -> Result<Self, LangfuseConfigError> {
        let environment = collect_environment();
        resolve(settings, &environment)
    }
}

pub fn resolve(
    settings: &serde_json::Value,
    environment: &BTreeMap<String, String>,
) -> Result<LangfuseConfig, LangfuseConfigError> {
    let langfuse = settings.get("langfuse");
    let mut config = LangfuseConfig::default();

    if let Some(value) = langfuse
        .and_then(|settings| settings.get("trace_sampling"))
        .and_then(serde_json::Value::as_f64)
    {
        config.trace_sampling = value.clamp(0.0, 1.0);
    }
    if let Some(value) = langfuse
        .and_then(|settings| settings.get("error_span_always"))
        .and_then(serde_json::Value::as_bool)
    {
        config.error_span_always = value;
    }
    if let Some(value) = langfuse
        .and_then(|settings| settings.get("batch_max_events"))
        .and_then(serde_json::Value::as_u64)
    {
        config.batch_max_events = value as usize;
    }
    if let Some(value) = langfuse
        .and_then(|settings| settings.get("batch_flush_interval_secs"))
        .and_then(serde_json::Value::as_u64)
    {
        config.batch_flush_interval_secs = value;
    }

    if let Some(value) = environment.get("LANGFUSE_PUBLIC_KEY") {
        config.public_key = Some(value.clone());
    }
    if let Some(value) = environment.get("LANGFUSE_SECRET_KEY") {
        config.secret_key = Some(value.clone());
    }
    if let Some(value) = environment.get("LANGFUSE_BASE_URL") {
        config.host = value.clone();
    }
    if let Some(value) = environment
        .get("LANGFUSE_TRACE_SAMPLING")
        .and_then(|value| value.parse::<f64>().ok())
    {
        config.trace_sampling = value.clamp(0.0, 1.0);
    }
    if let Some(value) = environment.get("LANGFUSE_ERROR_SPAN_ALWAYS") {
        config.error_span_always = value.to_lowercase() != "false" && value != "0";
    }
    if let Some(value) = environment
        .get("LANGFUSE_BATCH_MAX_EVENTS")
        .and_then(|value| value.parse::<usize>().ok())
    {
        config.batch_max_events = value;
    }
    if let Some(value) = environment
        .get("LANGFUSE_BATCH_FLUSH_INTERVAL")
        .and_then(|value| value.parse::<u64>().ok())
    {
        config.batch_flush_interval_secs = value;
    }
    if let Some(value) = environment.get("LANGFUSE_USER_ID") {
        config.user_id = Some(value.clone());
    }

    for (setting, variable, target) in [
        (
            "batch_queue_capacity",
            "LANGFUSE_BATCH_QUEUE_CAPACITY",
            &mut config.batch_queue_capacity,
        ),
        (
            "batch_max_in_flight",
            "LANGFUSE_BATCH_MAX_IN_FLIGHT",
            &mut config.batch_max_in_flight,
        ),
        (
            "batch_max_event_bytes",
            "LANGFUSE_BATCH_MAX_EVENT_BYTES",
            &mut config.batch_max_event_bytes,
        ),
        (
            "batch_max_bytes",
            "LANGFUSE_BATCH_MAX_BYTES",
            &mut config.batch_max_bytes,
        ),
        (
            "batch_max_queue_bytes",
            "LANGFUSE_BATCH_MAX_QUEUE_BYTES",
            &mut config.batch_max_queue_bytes,
        ),
    ] {
        if let Some(value) = langfuse
            .and_then(|settings| settings.get(setting))
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        {
            *target = value;
        }
        if let Some(value) = environment
            .get(variable)
            .and_then(|value| value.parse::<usize>().ok())
        {
            *target = value;
        }
    }

    // 新键的胜出值非法时整体失败：不隐退回退，也不发布部分结果。
    spool::resolve_into(&mut config, langfuse, environment)?;

    Ok(config)
}

fn collect_environment() -> BTreeMap<String, String> {
    crate::source::read_environment(ENVIRONMENT_KEYS).unwrap_or_default()
}

#[cfg(test)]
#[path = "observability_test.rs"]
mod tests;
