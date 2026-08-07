/// Langfuse 配置（来自环境变量 + settings.json）
#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
    pub host: String,
    /// 采样率（0.0-1.0），默认 1.0（全量采样）
    pub trace_sampling: f64,
    /// 是否始终为错误轮次发送 ErrorTurn span（即使未采样）
    pub error_span_always: bool,
    /// Batcher 单次批量最大事件数
    pub batch_max_events: usize,
    /// Batcher 自动 flush 间隔（秒）
    pub batch_flush_interval_secs: u64,
    /// 自定义 user 维度（LANGFUSE_USER_ID 环境变量），None 表示不设置
    pub user_id: Option<String>,
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
            batch_flush_interval_secs: 10,
            user_id: None,
        }
    }
}

impl LangfuseConfig {
    /// 从环境变量读取配置，任一必填字段缺失则返回 None（静默禁用）
    ///
    /// 环境变量：
    ///   LANGFUSE_PUBLIC_KEY          - 必填
    ///   LANGFUSE_SECRET_KEY          - 必填
    ///   LANGFUSE_BASE_URL            - 可选，默认 https://cloud.langfuse.com
    ///   LANGFUSE_TRACE_SAMPLING      - 可选，默认 1.0
    ///   LANGFUSE_ERROR_SPAN_ALWAYS   - 可选，默认 true
    ///   LANGFUSE_BATCH_MAX_EVENTS    - 可选，默认 50
    ///   LANGFUSE_BATCH_FLUSH_INTERVAL - 可选，默认 10
    ///   LANGFUSE_USER_ID             - 可选，自定义 user 维度标识
    pub fn from_env() -> Option<Self> {
        let public_key = std::env::var("LANGFUSE_PUBLIC_KEY").ok()?;
        let secret_key = std::env::var("LANGFUSE_SECRET_KEY").ok()?;
        let host = std::env::var("LANGFUSE_BASE_URL")
            .unwrap_or_else(|_| "https://cloud.langfuse.com".to_string());
        let trace_sampling = std::env::var("LANGFUSE_TRACE_SAMPLING")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        let error_span_always = std::env::var("LANGFUSE_ERROR_SPAN_ALWAYS")
            .ok()
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);
        let batch_max_events = std::env::var("LANGFUSE_BATCH_MAX_EVENTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(50);
        let batch_flush_interval_secs = std::env::var("LANGFUSE_BATCH_FLUSH_INTERVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10);
        let user_id = std::env::var("LANGFUSE_USER_ID").ok();
        Some(Self {
            public_key: Some(public_key),
            secret_key: Some(secret_key),
            host,
            trace_sampling,
            error_span_always,
            batch_max_events,
            batch_flush_interval_secs,
            user_id,
        })
    }

    /// 从 settings.json 加载配置，环境变量优先于 settings.json。
    ///
    /// settings.json 中的路径：
    /// ```json
    /// { "langfuse": { "trace_sampling": 0.5, "error_span_always": false, ... } }
    /// ```
    ///
    /// 环境变量（`LANGFUSE_*`）会覆盖 settings.json 中对应的值。
    /// 始终返回 Self（所有字段都有默认值）。
    pub fn load_with_settings(settings_json: &serde_json::Value) -> Self {
        let mut cfg = Self::default();

        // 1. 从 settings.json 读取 langfuse.* 字段
        if let Some(langfuse) = settings_json.get("langfuse") {
            if let Some(v) = langfuse.get("trace_sampling").and_then(|v| v.as_f64()) {
                cfg.trace_sampling = v.clamp(0.0, 1.0);
            }
            if let Some(v) = langfuse.get("error_span_always").and_then(|v| v.as_bool()) {
                cfg.error_span_always = v;
            }
            if let Some(v) = langfuse.get("batch_max_events").and_then(|v| v.as_u64()) {
                cfg.batch_max_events = v as usize;
            }
            if let Some(v) = langfuse
                .get("batch_flush_interval_secs")
                .and_then(|v| v.as_u64())
            {
                cfg.batch_flush_interval_secs = v;
            }
        }

        // 2. 环境变量覆盖（优先）
        if let Ok(v) = std::env::var("LANGFUSE_PUBLIC_KEY") {
            cfg.public_key = Some(v);
        }
        if let Ok(v) = std::env::var("LANGFUSE_SECRET_KEY") {
            cfg.secret_key = Some(v);
        }
        if let Ok(v) = std::env::var("LANGFUSE_BASE_URL") {
            cfg.host = v;
        }
        if let Ok(v) = std::env::var("LANGFUSE_TRACE_SAMPLING") {
            if let Ok(r) = v.parse::<f64>() {
                cfg.trace_sampling = r.clamp(0.0, 1.0);
            }
        }
        if let Ok(v) = std::env::var("LANGFUSE_ERROR_SPAN_ALWAYS") {
            cfg.error_span_always = v.to_lowercase() != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LANGFUSE_BATCH_MAX_EVENTS") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.batch_max_events = n;
            }
        }
        if let Ok(v) = std::env::var("LANGFUSE_BATCH_FLUSH_INTERVAL") {
            if let Ok(n) = v.parse::<u64>() {
                cfg.batch_flush_interval_secs = n;
            }
        }
        if let Ok(v) = std::env::var("LANGFUSE_USER_ID") {
            cfg.user_id = Some(v);
        }

        cfg
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
