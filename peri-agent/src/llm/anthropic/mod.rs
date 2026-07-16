mod adapter;
mod cache;
mod invoke;
mod stream;

use crate::llm::build_reqwest_client;

/// ChatAnthropic - Anthropic Messages API 实现
pub struct ChatAnthropic {
    adapter: adapter::AnthropicAdapter,
    client: reqwest::Client,
}

impl ChatAnthropic {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            adapter: adapter::AnthropicAdapter {
                api_key: api_key.into(),
                model: model.into(),
                base_url: None,
                extended_thinking: false,
                thinking_budget: 10000,
                thinking_effort: "medium".to_string(),
                enable_cache: true,
                max_tokens: 32000,
            },
            client: build_reqwest_client(),
        }
    }

    /// 设置自定义 base URL（用于代理或兼容 API）
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let url = base_url.into();
        self.adapter.base_url = if url.is_empty() { None } else { Some(url) };
        self
    }

    /// 开启 Extended Thinking（claude-3-7-sonnet 及以上）
    pub fn with_extended_thinking(mut self, budget_tokens: u32, effort: impl Into<String>) -> Self {
        self.adapter.extended_thinking = true;
        self.adapter.thinking_budget = budget_tokens;
        self.adapter.thinking_effort = effort.into();
        self
    }

    /// 关闭 Prompt Caching
    pub fn without_cache(mut self) -> Self {
        self.adapter.enable_cache = false;
        self
    }

    /// 设置最大输出 token 数
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.adapter.max_tokens = max_tokens;
        self
    }

    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        let model = std::env::var("ANTHROPIC_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
        let mut s = Self::new(api_key, model);
        if let Ok(url) = std::env::var("ANTHROPIC_BASE_URL") {
            s = s.with_base_url(url);
        }
        Some(s)
    }

    // ─── Thin wrappers（兼容测试文件中的关联函数调用语法）───

    #[cfg(test)]
    fn messages_to_anthropic(
        messages: &[crate::messages::BaseMessage],
    ) -> (Vec<serde_json::Value>, Vec<cache::SystemPromptBlock>) {
        // Delegate to adapter's private function via a bridge
        adapter::messages_to_anthropic(messages)
    }

    #[cfg(test)]
    fn parse_content_blocks(
        raw_blocks: &[serde_json::Value],
    ) -> (
        Vec<crate::messages::ContentBlock>,
        Vec<crate::messages::ToolCallRequest>,
    ) {
        adapter::AnthropicAdapter::parse_content_blocks(raw_blocks)
    }

    #[cfg(test)]
    fn build_system_blocks_json(blocks: &[cache::SystemPromptBlock]) -> Vec<serde_json::Value> {
        adapter::build_system_blocks_json(blocks)
    }

    // ─── 字段访问 wrappers（兼容测试直接字段访问）───

    #[cfg(test)]
    pub(crate) fn base_url(&self) -> &Option<String> {
        &self.adapter.base_url
    }

    #[cfg(test)]
    pub(crate) fn extended_thinking(&self) -> bool {
        self.adapter.extended_thinking
    }

    #[cfg(test)]
    pub(crate) fn thinking_budget(&self) -> u32 {
        self.adapter.thinking_budget
    }

    #[cfg(test)]
    pub(crate) fn thinking_effort(&self) -> &str {
        &self.adapter.thinking_effort
    }

    #[cfg(test)]
    pub(crate) fn enable_cache(&self) -> bool {
        self.adapter.enable_cache
    }
}

#[cfg(test)]
#[path = "../anthropic_test.rs"]
mod tests;
