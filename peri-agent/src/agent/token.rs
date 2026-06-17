use crate::llm::types::TokenUsage;

/// 会话级 token 用量追踪器
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenTracker {
    /// 累计输入 token（含 cache_read + cache_creation）
    pub total_input_tokens: u64,
    /// 累计输出 token
    pub total_output_tokens: u64,
    /// 累计 cache_creation token
    pub total_cache_creation_tokens: u64,
    /// 累计 cache_read token
    pub total_cache_read_tokens: u64,
    /// 最近一次 LLM 响应的 usage（用于估算当前上下文大小）
    pub last_usage: Option<TokenUsage>,
    /// 已完成的 LLM 调用次数
    pub llm_call_count: u32,
    /// 最近一次 LLM 响应的 API request ID
    pub last_request_id: Option<String>,
    /// 每次 LLM 请求的 token 用量历史（仅内存，不持久化）
    #[serde(skip)]
    pub request_history: Vec<RequestRecord>,
    /// 自上次 LLM 调用以来累积的工具结果 token 估算（P0-5）
    ///
    /// 工具结果在两次 LLM 调用之间被静默注入，Tracker 通过 LLM usage 无法感知。
    /// 此字段单独累积工具结果的字符级估算（chars / 4），用于上下文预算预警。
    /// **不可污染 `last_usage`**（那是 LLM API 的精确值，混入估算会破坏显示精度）。
    /// 每次 LLM `accumulate` 时清零（工具结果已被下一轮 input_tokens 包含）。
    pub estimated_tool_tokens_since_last_llm: u64,
}

impl TokenTracker {
    pub fn accumulate(&mut self, usage: &TokenUsage) {
        self.request_history.push(RequestRecord::from_usage(usage));
        // 防止长时间会话中 request_history 无限增长
        if self.request_history.len() > 1000 {
            let excess = self.request_history.len() - 1000;
            self.request_history.drain(0..excess);
        }
        self.total_input_tokens += usage.input_tokens as u64;
        self.total_output_tokens += usage.output_tokens as u64;
        if let Some(v) = usage.cache_creation_input_tokens {
            self.total_cache_creation_tokens += v as u64;
        }
        if let Some(v) = usage.cache_read_input_tokens {
            self.total_cache_read_tokens += v as u64;
        }
        // 只在 input_tokens > 0 时更新 last_usage，
        // 防止异常 API 响应（input_tokens=0）覆盖正常的上下文估算
        if usage.input_tokens > 0 {
            self.last_usage = Some(usage.clone());
        }
        self.llm_call_count += 1;
        self.last_request_id = usage.request_id.clone();
        // 工具结果 token 已被本轮 input_tokens 包含（作为 tool_result 消息），清零避免双计
        self.estimated_tool_tokens_since_last_llm = 0;
    }

    /// 累积工具结果 token 估算（P0-5）。
    ///
    /// 在 `dispatch_tools` 写入 tool_result 后调用，用 `chars().count() / 4` 近似估算。
    /// 不能与 LLM usage 混用——这是字符级估算，仅用于预算预警。
    pub fn add_estimated_tool_tokens(&mut self, tool_output: &str) {
        // 经验估算：英文 ~4 字符/token，CJK 略多但保守取 4
        let estimated = (tool_output.chars().count() / 4) as u64;
        self.estimated_tool_tokens_since_last_llm = self
            .estimated_tool_tokens_since_last_llm
            .saturating_add(estimated);
    }

    pub fn estimated_context_tokens(&self) -> Option<u64> {
        // input_tokens 已在 adapter 层规范化为总输入（含缓存 token），
        // 即当前 prompt 的实际大小，直接反映上下文窗口占用。
        // 不加 output_tokens：output 会在下一轮 API 调用中包含进 input_tokens，
        // 相加会导致双重计算，使显示用量约为实际的 2 倍。
        // 加上 estimated_tool_tokens_since_last_llm：本轮已写入但尚未被 LLM 感知的工具结果（P0-5）
        self.last_usage
            .as_ref()
            .map(|u| u.input_tokens as u64 + self.estimated_tool_tokens_since_last_llm)
    }

    pub fn context_usage_percent(&self, context_window: u32) -> Option<f64> {
        self.estimated_context_tokens()
            .map(|used| (used as f64 / context_window as f64) * 100.0)
    }

    /// 当次调用的缓存命中率（基于 last_usage）
    ///
    /// 返回最近一次 LLM 调用的缓存效率，当无缓存数据时返回 0.0。
    pub fn cache_hit_rate(&self) -> f64 {
        self.last_usage
            .as_ref()
            .map(|u| {
                let cache_read = u.cache_read_input_tokens.unwrap_or(0);
                if u.input_tokens == 0 {
                    return 0.0;
                }
                cache_read as f64 / u.input_tokens as f64
            })
            .unwrap_or(0.0)
    }

    /// 重置追踪器（compact 后调用）
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// 单次 LLM 请求的 token 用量快照（仅内存，不持久化）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RequestRecord {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
}

impl RequestRecord {
    pub fn from_usage(usage: &TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
            cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
        }
    }

    /// 当次请求的缓存命中率
    pub fn cache_hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        self.cache_read_input_tokens as f64 / self.input_tokens as f64
    }
}

/// 上下文窗口预算配置
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextBudget {
    /// 模型的上下文窗口大小（token 数）
    pub context_window: u32,
    /// auto-compact 触发阈值（百分比，0.0-1.0）
    pub auto_compact_threshold: f64,
    /// 警告阈值（百分比，0.0-1.0）
    pub warning_threshold: f64,
}

impl ContextBudget {
    pub const DEFAULT_CONTEXT_WINDOW: u32 = 200_000;
    pub const DEFAULT_AUTO_COMPACT_THRESHOLD: f64 = 0.85;
    pub const DEFAULT_WARNING_THRESHOLD: f64 = 0.70;

    pub fn new(context_window: u32) -> Self {
        Self {
            context_window,
            auto_compact_threshold: Self::DEFAULT_AUTO_COMPACT_THRESHOLD,
            warning_threshold: Self::DEFAULT_WARNING_THRESHOLD,
        }
    }

    pub fn should_auto_compact(&self, tracker: &TokenTracker) -> bool {
        match tracker.context_usage_percent(self.context_window) {
            Some(pct) => pct / 100.0 >= self.auto_compact_threshold,
            None => false,
        }
    }

    pub fn should_warn(&self, tracker: &TokenTracker) -> bool {
        match tracker.context_usage_percent(self.context_window) {
            Some(pct) => pct / 100.0 >= self.warning_threshold,
            None => false,
        }
    }

    pub fn with_auto_compact_threshold(mut self, threshold: f64) -> Self {
        self.auto_compact_threshold = threshold;
        self
    }

    pub fn with_warning_threshold(mut self, threshold: f64) -> Self {
        self.warning_threshold = threshold;
        self
    }
}

#[cfg(test)]
#[path = "token_test.rs"]
mod tests;
