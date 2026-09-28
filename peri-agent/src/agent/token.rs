use peri_model::TokenUsage;

/// 标识一次可供自动 Compact 评估的上下文压力样本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PressureSampleKey {
    usage_generation: u64,
    tool_growth_generation: u64,
    view_growth_generation: u64,
}

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
    /// 最近一次有效 provider usage 的单调 generation。
    #[serde(default)]
    usage_generation: u64,
    /// 实际新增工具输出的单调 generation。
    #[serde(default)]
    tool_growth_generation: u64,
    /// 最近一次已消费的自动 Compact 压力样本。
    #[serde(skip)]
    consumed_pressure_sample: Option<PressureSampleKey>,
    /// 当前请求视图的近似大小；不伪装成 provider usage，也不跨恢复复用。
    #[serde(skip)]
    current_input_estimate: Option<u64>,
    /// 自最近有效 usage 后累计的正增长。Micro 缩减不抵扣后续新增工作。
    #[serde(skip)]
    unconfirmed_input_growth: u64,
    #[serde(skip)]
    pending_input_estimate: Option<u64>,
    #[serde(skip)]
    view_growth_generation: u64,
}

impl TokenTracker {
    pub fn accumulate(&mut self, usage: &TokenUsage) {
        let request_estimate = self.pending_input_estimate.take();
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
            self.usage_generation = self.usage_generation.saturating_add(1);
            self.unconfirmed_input_growth = self
                .current_input_estimate
                .zip(request_estimate)
                .map(|(current, sent)| current.saturating_sub(sent))
                .unwrap_or(0);
            // 只有新权威 input usage 已包含工具结果，才能清除本地预测。
            self.estimated_tool_tokens_since_last_llm = 0;
        }
        self.llm_call_count += 1;
    }

    /// 累积工具结果 token 估算（P0-5）。
    ///
    /// 在 `dispatch_tools` 写入 tool_result 后调用，用 `chars().count() / 4` 近似估算。
    /// 不能与 LLM usage 混用——这是字符级估算，仅用于预算预警。
    pub fn add_estimated_tool_tokens(&mut self, tool_output: &str) {
        // 字符启发式：不同语言和多模态成本可能偏差，不能当作真实 tokenizer。
        let estimated = (tool_output.chars().count() / 4) as u64;
        if tool_output.is_empty() {
            return;
        }
        self.tool_growth_generation = self.tool_growth_generation.saturating_add(1);
        self.estimated_tool_tokens_since_last_llm = self
            .estimated_tool_tokens_since_last_llm
            .saturating_add(estimated);
    }

    pub fn estimated_context_tokens(&self) -> Option<u64> {
        let Some(usage) = &self.last_usage else {
            return self.current_input_estimate;
        };
        // 权威 usage 只与产生它的请求视图比较：新增 assistant/user/reminder/
        // tool 内容都是增长；Micro 的估算减少不能证明真实预算已经恢复。
        // 工具提交后的即时预警和完整视图包含同一批工具，取 max 避免双计。
        Some(
            u64::from(usage.input_tokens).saturating_add(
                self.unconfirmed_input_growth
                    .max(self.estimated_tool_tokens_since_last_llm),
            ),
        )
    }

    /// 更新已提交模型视图；只有正增长重新激活压力样本，投影缩减不激活。
    pub(crate) fn refresh_input_estimate(&mut self, estimate: u64) -> bool {
        let grew = self
            .current_input_estimate
            .is_none_or(|previous| estimate > previous);
        if grew {
            self.view_growth_generation = self.view_growth_generation.saturating_add(1);
        }
        if self.last_usage.is_some() {
            // 有效 usage 只证明已发送快照的大小。投影缩减没有新 usage 确认，
            // 故只移动比较位置；随后新增内容仍必须加到权威压力上。
            if let Some(previous) = self.current_input_estimate {
                self.unconfirmed_input_growth = self
                    .unconfirmed_input_growth
                    .saturating_add(estimate.saturating_sub(previous));
            }
        }
        self.current_input_estimate = Some(estimate);
        grew
    }

    /// 在最终请求发出前绑定估算；零/缺失 usage 不会替换上一个有效基线。
    pub(crate) fn begin_request(&mut self, estimate: u64) {
        self.refresh_input_estimate(estimate);
        self.pending_input_estimate = Some(estimate);
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

    pub(crate) fn pressure_sample_key(&self) -> Option<PressureSampleKey> {
        if self.last_usage.is_none() && self.current_input_estimate.is_none() {
            return None;
        }
        Some(PressureSampleKey {
            usage_generation: self.usage_generation,
            tool_growth_generation: self.tool_growth_generation,
            view_growth_generation: self.view_growth_generation,
        })
    }

    pub(crate) fn consume_pressure_sample(&mut self, key: PressureSampleKey) {
        self.consumed_pressure_sample = Some(key);
    }

    pub(crate) fn is_pressure_sample_consumed(&self, key: PressureSampleKey) -> bool {
        self.consumed_pressure_sample == Some(key)
    }

    /// 重置 usage 计数，但保留单调 generation 与已消费样本身份。
    pub fn reset(&mut self) {
        let usage_generation = self.usage_generation;
        let tool_growth_generation = self.tool_growth_generation;
        let consumed_pressure_sample = self.consumed_pressure_sample;
        let view_growth_generation = self.view_growth_generation;
        *self = Self {
            usage_generation,
            tool_growth_generation,
            consumed_pressure_sample,
            view_growth_generation,
            ..Self::default()
        };
    }
}

// 编码后的字节数不是媒体 token 成本；缺少尺寸/页数和 provider tokenizer 时，
// 每个二进制块只放一个固定预算占位。此值不是成本上限，实际 usage 仍为权威。
const BINARY_MEDIA_ESTIMATED_TOKENS: u64 = 1_024;

/// 无 tokenizer 时按文字和有界媒体占位估算请求内容。它用于提前压缩，
/// 不替代 provider usage；媒体尺寸/页数、协议开销和语言密度仍是近似边界。
pub(crate) fn estimate_request_tokens(
    messages: &[crate::messages::BaseMessage],
    tools: &[&dyn crate::tools::BaseTool],
) -> u64 {
    use crate::messages::{BaseMessage, ContentBlock, MessageContent};
    let mut chars = 0u64;
    for message in messages {
        let canonical_calls = match message {
            BaseMessage::Ai { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        };
        chars = chars.saturating_add(match message.message_content() {
            MessageContent::Text(text) => text.chars().count() as u64,
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| estimate_block_chars(block, canonical_calls))
                .fold(0u64, u64::saturating_add),
            MessageContent::Raw(values) => values
                .iter()
                .map(
                    |value| match serde_json::from_value::<ContentBlock>(value.clone()) {
                        Ok(block) => estimate_block_chars(&block, canonical_calls),
                        Err(_) => value.to_string().chars().count() as u64,
                    },
                )
                .fold(0u64, u64::saturating_add),
        });
        if let BaseMessage::Ai { tool_calls, .. } = message {
            for call in tool_calls {
                chars = chars.saturating_add(call.name.chars().count() as u64);
                chars = chars.saturating_add(call.arguments.to_string().chars().count() as u64);
            }
        }
    }
    for tool in tools {
        chars = chars
            .saturating_add(tool.name().chars().count() as u64)
            .saturating_add(tool.description().chars().count() as u64)
            .saturating_add(tool.parameters().to_string().chars().count() as u64);
    }
    chars.div_ceil(4)
}

fn estimate_block_chars(
    block: &crate::messages::ContentBlock,
    canonical_calls: &[crate::messages::ToolCallRequest],
) -> u64 {
    use crate::messages::{ContentBlock, DocumentSource, ImageSource};
    match block {
        ContentBlock::Text { text } | ContentBlock::Reasoning { text, .. } => {
            text.chars().count() as u64
        }
        ContentBlock::Image { source } => match source {
            ImageSource::Base64 { .. } => BINARY_MEDIA_ESTIMATED_TOKENS * 4,
            ImageSource::Url { url } => estimate_media_url_chars(url),
        },
        ContentBlock::Document { source, title } => {
            let body = match source {
                DocumentSource::Base64 { .. } => BINARY_MEDIA_ESTIMATED_TOKENS * 4,
                DocumentSource::Text { text } => text.chars().count() as u64,
                DocumentSource::Url { url } => estimate_media_url_chars(url),
            };
            body.saturating_add(
                title
                    .as_ref()
                    .map_or(0, |title| title.chars().count() as u64),
            )
        }
        // 只有确实对应canonical call的块才是镜像；AI/serde恢复并不保证存在镜像。
        ContentBlock::ToolUse { id, name, input }
            if canonical_calls
                .iter()
                .any(|call| call.id == *id && call.name == *name && call.arguments == *input) =>
        {
            0
        }
        ContentBlock::ToolUse { name, input, .. } => {
            (name.chars().count() as u64).saturating_add(input.to_string().chars().count() as u64)
        }
        ContentBlock::ToolResult { content, .. } => content
            .iter()
            .map(|block| estimate_block_chars(block, &[]))
            .fold(0u64, u64::saturating_add),
        // 未知 JSON 没有可信媒体类型，保持文本预算，不能按 data 字段名删除成本。
        ContentBlock::Unknown(value) => value.to_string().chars().count() as u64,
    }
}

fn estimate_media_url_chars(url: &str) -> u64 {
    if url
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
    {
        // 内联媒体也可能经 URL 形式进入；不能让 data URI 重新绕回 base64 文本计量。
        BINARY_MEDIA_ESTIMATED_TOKENS * 4
    } else {
        url.chars().count() as u64
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
    /// 为模型输出预留的 token 数（默认 8192）
    pub output_reserve: u32,
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
            output_reserve: context_window / 25, // ~4% 预留
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
