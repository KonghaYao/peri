use std::collections::BTreeMap;
use std::fmt;

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
];

#[derive(Clone)]
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
}

impl fmt::Debug for LangfuseConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LangfuseConfig")
            .field(
                "public_key",
                &self.public_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "secret_key",
                &self.secret_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("host", &"[REDACTED]")
            .field("trace_sampling", &self.trace_sampling)
            .field("error_span_always", &self.error_span_always)
            .field("batch_max_events", &self.batch_max_events)
            .field("batch_queue_capacity", &self.batch_queue_capacity)
            .field("batch_max_in_flight", &self.batch_max_in_flight)
            .field("batch_max_event_bytes", &self.batch_max_event_bytes)
            .field("batch_max_bytes", &self.batch_max_bytes)
            .field("batch_max_queue_bytes", &self.batch_max_queue_bytes)
            .field("batch_flush_interval_secs", &self.batch_flush_interval_secs)
            .field("user_id", &self.user_id.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
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
            batch_max_event_bytes: 512 * 1024,
            batch_max_bytes: 4 * 1024 * 1024,
            batch_max_queue_bytes: 16 * 1024 * 1024,
            batch_flush_interval_secs: 10,
            user_id: None,
        }
    }
}

impl LangfuseConfig {
    pub fn from_env() -> Option<Self> {
        let config = Self::load_with_settings(&serde_json::Value::Null);
        Some(config).filter(|config| config.public_key.is_some() && config.secret_key.is_some())
    }

    pub fn load_with_settings(settings: &serde_json::Value) -> Self {
        let environment = collect_environment();
        resolve(settings, &environment)
    }
}

pub fn resolve(
    settings: &serde_json::Value,
    environment: &BTreeMap<String, String>,
) -> LangfuseConfig {
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

    config
}

fn collect_environment() -> BTreeMap<String, String> {
    crate::source::read_environment(ENVIRONMENT_KEYS).unwrap_or_default()
}

#[cfg(test)]
#[path = "observability_test.rs"]
mod tests;
