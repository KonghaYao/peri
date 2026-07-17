mod adapter;
mod invoke;
mod stream;

#[cfg(test)]
use serde_json::Value;

use crate::llm::build_reqwest_client;

/// ChatOpenAI - OpenAI 兼容 API 的 LLM 实现
pub struct ChatOpenAI {
    adapter: adapter::OpenAiAdapter,
    client: reqwest::Client,
}

impl ChatOpenAI {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        let supports_thinking_content = Self::detect_thinking_content_support(&model);
        Self {
            adapter: adapter::OpenAiAdapter {
                api_key: api_key.into(),
                base_url: "https://api.openai.com/v1".to_string(),
                model,
                reasoning_effort: None,
                thinking_enabled: false,
                supports_thinking_content,
                max_tokens: 32000,
            },
            client: build_reqwest_client(),
        }
    }

    /// 设置 API Base URL。OpenAI Base URL 需要 `/v1` 后缀。
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.adapter.base_url = base_url.into();
        self
    }

    /// 开启 reasoning effort（o1/o3 系列）
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.adapter.reasoning_effort = Some(effort.into());
        self
    }

    /// 开启 DeepSeek thinking 模式（deepseek-v4-pro 等）
    pub fn with_thinking_enabled(mut self) -> Self {
        self.adapter.thinking_enabled = true;
        self
    }

    /// 手动控制是否在 content 中回传 `thinking` 类型的 Reasoning 块
    pub fn with_thinking_content(mut self, enabled: bool) -> Self {
        self.adapter.supports_thinking_content = enabled;
        self
    }

    /// 设置最大输出 token 数
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.adapter.max_tokens = max_tokens;
        self
    }

    fn detect_thinking_content_support(model: &str) -> bool {
        let _ = model;
        false
    }

    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("OPENAI_API_KEY").ok()?;
        let base_url = std::env::var("OPENAI_API_BASE")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = std::env::var("OPENAI_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "gpt-4o".to_string());
        Some(Self::new(api_key, model).with_base_url(base_url))
    }

    /// 模型的上下文窗口大小（token 数）
    fn context_window_inner(&self) -> u32 {
        200_000
    }

    // ─── Thin wrappers（兼容测试文件中的关联函数/方法调用语法）───

    #[cfg(test)]
    pub(crate) fn content_to_openai(
        content: &crate::messages::MessageContent,
        supports_thinking_content: bool,
    ) -> Value {
        adapter::content_to_openai(content, supports_thinking_content)
    }

    #[cfg(test)]
    pub(crate) fn messages_to_json(&self, messages: &[crate::messages::BaseMessage]) -> Vec<Value> {
        adapter::messages_to_json(&self.adapter, messages)
    }

    #[cfg(test)]
    pub(crate) fn parse_assistant_message(
        assistant_msg: &Value,
        stop_reason: &crate::llm::types::StopReason,
    ) -> crate::messages::BaseMessage {
        adapter::parse_assistant_message(assistant_msg, stop_reason)
    }

    // ─── 字段访问 wrappers（兼容测试直接字段访问）───

    #[cfg(test)]
    pub(crate) fn supports_thinking_content(&self) -> bool {
        self.adapter.supports_thinking_content
    }

    #[cfg(test)]
    pub(crate) fn base_url(&self) -> &str {
        &self.adapter.base_url
    }

    #[cfg(test)]
    pub(crate) fn reasoning_effort(&self) -> &Option<String> {
        &self.adapter.reasoning_effort
    }

    #[cfg(test)]
    pub(crate) fn thinking_enabled(&self) -> bool {
        self.adapter.thinking_enabled
    }
}

#[cfg(test)]
#[path = "../openai_test.rs"]
mod tests;
