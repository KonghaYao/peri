//! LLM Provider and model configuration.
//!
//! Manages provider configuration, model alias resolution, and LLM factory creation.
//! Decoupled from TUI-specific types.

pub mod config;
pub mod store;

pub use config::{AppConfig, PeriConfig, ProviderConfig, ProviderModels, ThinkingConfig};
use peri_model::{AnthropicConfig, AnthropicModel, OpenAiConfig, OpenAiModel};
pub use store::{config_path, load, load_from, save, save_to, workspace_config_path};
use url::Url;

#[derive(Clone)]
pub enum LlmProvider {
    /// OpenAI 兼容 Provider。`base_url` 需要 `/v1` 后缀。
    OpenAi {
        api_key: String,
        base_url: String,
        model: String,
        thinking: Option<ThinkingConfig>,
    },
    Anthropic {
        api_key: String,
        model: String,
        base_url: Option<String>,
        thinking: Option<ThinkingConfig>,
    },
}

impl LlmProvider {
    pub fn from_env() -> Option<Self> {
        let provider_hint = std::env::var("MODEL_PROVIDER").unwrap_or_default();

        match provider_hint.to_lowercase().as_str() {
            "anthropic" => {
                let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
                let model = std::env::var("ANTHROPIC_MODEL")
                    .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
                let base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
                Some(Self::Anthropic {
                    api_key,
                    model,
                    base_url,
                    thinking: None,
                })
            }
            "openai" | "" => {
                if provider_hint.is_empty() {
                    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
                        let model = std::env::var("ANTHROPIC_MODEL")
                            .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
                        let base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
                        return Some(Self::Anthropic {
                            api_key,
                            model,
                            base_url,
                            thinking: None,
                        });
                    }
                }
                let api_key = std::env::var("OPENAI_API_KEY").ok()?;
                let base_url = std::env::var("OPENAI_API_BASE")
                    .or_else(|_| std::env::var("OPENAI_BASE_URL"))
                    .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
                let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
                Some(Self::OpenAi {
                    api_key,
                    base_url,
                    model,
                    thinking: None,
                })
            }
            _ => {
                let api_key = std::env::var("OPENAI_API_KEY").ok()?;
                let base_url = std::env::var("OPENAI_API_BASE")
                    .or_else(|_| std::env::var("OPENAI_BASE_URL"))
                    .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
                let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
                Some(Self::OpenAi {
                    api_key,
                    base_url,
                    model,
                    thinking: None,
                })
            }
        }
    }

    /// 从 PeriConfig 构造 LlmProvider（按 active_provider_id 查找 Provider，再按 active_alias 取模型名）
    pub fn from_config(cfg: &config::PeriConfig) -> Option<Self> {
        let app = &cfg.config;
        let provider = app
            .providers
            .iter()
            .find(|p| p.id == app.active_provider_id)?;

        if provider.api_key.is_empty() {
            return None;
        }

        let alias = app.active_alias.as_str();
        let model = provider
            .models
            .get_model(alias)
            .filter(|m| !m.is_empty())
            .map(|m| m.to_string())
            .unwrap_or_else(|| match provider.provider_type.as_str() {
                "anthropic" => "claude-sonnet-4-6".to_string(),
                _ => "gpt-4o".to_string(),
            });

        let thinking = app.thinking.clone().filter(|t| t.enabled);

        match provider.provider_type.as_str() {
            "anthropic" => Some(Self::Anthropic {
                api_key: provider.api_key.clone(),
                model,
                base_url: if provider.base_url.is_empty() {
                    None
                } else {
                    Some(provider.base_url.clone())
                },
                thinking,
            }),
            _ => Some(Self::OpenAi {
                api_key: provider.api_key.clone(),
                base_url: if provider.base_url.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    provider.base_url.clone()
                },
                model,
                thinking,
            }),
        }
    }

    /// 从 PeriConfig 按指定 alias（如 "haiku"/"sonnet"/"opus"）构造 LlmProvider
    /// 大小写不敏感；未知 alias fallback 到默认模型
    pub fn from_config_for_alias(cfg: &config::PeriConfig, alias: &str) -> Option<Self> {
        let app = &cfg.config;
        let provider = app
            .providers
            .iter()
            .find(|p| p.id == app.active_provider_id)?;

        if provider.api_key.is_empty() {
            return None;
        }

        let model = provider
            .models
            .get_model(alias)
            .filter(|m| !m.is_empty())
            .map(|m| m.to_string())
            .unwrap_or_else(|| match provider.provider_type.as_str() {
                "anthropic" => "claude-sonnet-4-6".to_string(),
                _ => "gpt-4o".to_string(),
            });

        let thinking = app.thinking.clone().filter(|t| t.enabled);

        match provider.provider_type.as_str() {
            "anthropic" => Some(Self::Anthropic {
                api_key: provider.api_key.clone(),
                model,
                base_url: if provider.base_url.is_empty() {
                    None
                } else {
                    Some(provider.base_url.clone())
                },
                thinking,
            }),
            _ => Some(Self::OpenAi {
                api_key: provider.api_key.clone(),
                base_url: if provider.base_url.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    provider.base_url.clone()
                },
                model,
                thinking,
            }),
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::OpenAi { .. } => "OpenAI",
            Self::Anthropic { .. } => "Anthropic",
        }
    }

    pub fn model_name(&self) -> &str {
        match self {
            Self::OpenAi { model, .. } => model,
            Self::Anthropic { model, .. } => model,
        }
    }

    /// 替换模型名，保持其他配置不变
    pub fn with_model_name(&self, model: String) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::OpenAi { model: m, .. } => *m = model,
            Self::Anthropic { model: m, .. } => *m = model,
        }
        clone
    }

    /// 获取模型的上下文窗口大小（不消费 self）。
    ///
    /// 历史实现通过 `into_model().context_window()` 取值，OpenAI 与 Anthropic
    /// provider 均返回 200_000；`peri_model::Model` 不暴露 context_window，
    /// 此处保持配置级常量语义（1M 窗口由 builder 侧 `context_1m` 标志覆盖）。
    pub fn context_window(&self) -> u32 {
        200_000
    }

    /// 返回 thinking 配置的稳定标识，用于 fingerprint。
    ///
    /// 格式：`:think=<effort>:<budget_tokens>`，无 thinking 时返回空字符串。
    /// 不包含 max_tokens（max_tokens 由 into_model() 统一从 thinking.as_ref().map_or(32000, |t| t.max_tokens) 读取，
    /// 且 32000 硬编码不会成为区分因子）。
    pub fn thinking_key(&self) -> String {
        let thinking = match self {
            Self::OpenAi { thinking, .. } => thinking,
            Self::Anthropic { thinking, .. } => thinking,
        };
        match thinking {
            Some(ref t) if t.enabled => format!(":think={}:{}", t.effort, t.budget_tokens),
            _ => String::new(),
        }
    }

    pub fn into_model(self) -> Box<dyn peri_model::Model> {
        match self {
            Self::OpenAi {
                api_key,
                base_url,
                model,
                thinking,
            } => {
                let endpoint =
                    parse_endpoint(&base_url, "https://api.openai.com/v1", "openai base_url");
                let mut config = OpenAiConfig::new(endpoint, api_key, model);
                if let Some(ref t) = thinking {
                    config = config.with_reasoning_effort(t.openai_effort());
                    if t.enabled {
                        config = config.with_thinking_enabled(true);
                    }
                }
                let max_tokens = thinking.as_ref().map_or(32000, |t| t.max_tokens);
                config = config.with_max_tokens(max_tokens);
                Box::new(OpenAiModel::new(config))
            }
            Self::Anthropic {
                api_key,
                model,
                base_url,
                thinking,
            } => {
                let endpoint = match base_url {
                    Some(url) => {
                        parse_endpoint(&url, "https://api.anthropic.com", "anthropic base_url")
                    }
                    None => Url::parse("https://api.anthropic.com").expect("静态默认 endpoint"),
                };
                let mut config = AnthropicConfig::new(endpoint, api_key, model);
                if let Some(ref t) = thinking {
                    config = config.with_extended_thinking(t.budget_tokens, &t.effort);
                }
                let max_tokens = thinking.as_ref().map_or(32000, |t| t.max_tokens);
                config = config.with_max_tokens(max_tokens);
                Box::new(AnthropicModel::new(config))
            }
        }
    }
}

/// 解析 provider endpoint；非法 URL 时记录告警并回落到默认值，
/// 保持 provider 构造期不失败（fail-soft）的语义。
/// 真正无效的 endpoint 会在 prepare/stream 时由 `peri-model` 返回
/// `InvalidEndpoint` 错误（fail closed）。
fn parse_endpoint(raw: &str, fallback: &str, label: &str) -> Url {
    Url::parse(raw).unwrap_or_else(|error| {
        tracing::warn!(
            %error,
            %label,
            raw,
            "provider endpoint 非法，回落到默认 endpoint"
        );
        Url::parse(fallback).expect("默认 endpoint 必须可解析")
    })
}

#[cfg(test)]
#[path = "provider_test.rs"]
mod tests;
