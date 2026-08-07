//! LLM Provider and model configuration.
//!
//! Manages provider configuration, model alias resolution, and LLM factory creation.
//! Decoupled from TUI-specific types.

pub mod config;
pub mod store;

use std::sync::Arc;

pub use config::{AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels};
use peri_model::{AnthropicConfig, AnthropicModel, OpenAiConfig, OpenAiModel};
pub use store::{
    config_path, load, load_from, save, save_to, set_global_config_path, workspace_config_path,
};
use url::Url;

const MIN_ANTHROPIC_THINKING_BUDGET: u32 = 1_024;
const DEFAULT_ANTHROPIC_THINKING_BUDGET: u32 = 10_000;

#[derive(Clone)]
pub enum LlmProvider {
    /// OpenAI 兼容 Provider。`base_url` 需要 `/v1` 后缀。
    OpenAi {
        api_key: String,
        base_url: String,
        model: String,
        /// 思考强度 "low".."max"；None 表示不启用 extended thinking
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    },
    Anthropic {
        api_key: String,
        model: String,
        base_url: Option<String>,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
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
                    effort: None,
                    max_tokens: 32000,
                    context_1m: false,
                    retry_observer: None,
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
                            effort: None,
                            max_tokens: 32000,
                            context_1m: false,
                            retry_observer: None,
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
                    effort: None,
                    max_tokens: 32000,
                    context_1m: false,
                    retry_observer: None,
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
                    effort: None,
                    max_tokens: 32000,
                    context_1m: false,
                    retry_observer: None,
                })
            }
        }
    }

    /// 从 PeriConfig 按 active_alias 对应的 Profile 构造 LlmProvider
    pub fn from_config(cfg: &config::PeriConfig) -> Option<Self> {
        Self::from_config_for_alias(cfg, &cfg.config.active_alias)
    }

    /// 从 PeriConfig 按指定档位（"fable"/"opus"/"sonnet"/"haiku"）构造 LlmProvider。
    /// Profile 是唯一事实源：provider/model/effort/max_tokens/context_1m 全部取自
    /// `profiles[alias]`，model 空时回退 provider.models 同档位映射（fable 空回退 opus）。
    pub fn from_config_for_alias(cfg: &config::PeriConfig, alias: &str) -> Option<Self> {
        let app = &cfg.config;
        let (provider, profile) = resolve_profile(app, alias)?;

        if provider.api_key.is_empty() {
            return None;
        }

        let model = resolve_model_name(provider, alias, profile);
        let effort = Some(profile.effort.clone());
        let max_tokens = profile.max_tokens;
        let context_1m = profile.context_1m;

        match provider.provider_type.as_str() {
            "anthropic" => Some(Self::Anthropic {
                api_key: provider.api_key.clone(),
                model,
                base_url: if provider.base_url.is_empty() {
                    None
                } else {
                    Some(provider.base_url.clone())
                },
                effort,
                max_tokens,
                context_1m,
                retry_observer: None,
            }),
            _ => Some(Self::OpenAi {
                api_key: provider.api_key.clone(),
                base_url: if provider.base_url.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    provider.base_url.clone()
                },
                model,
                effort,
                max_tokens,
                context_1m,
                retry_observer: None,
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

    pub fn context_1m(&self) -> bool {
        match self {
            Self::OpenAi { context_1m, .. } | Self::Anthropic { context_1m, .. } => *context_1m,
        }
    }

    /// 思考强度稳定标识，用于 fingerprint；None 时返回空字符串
    pub fn effort_key(&self) -> String {
        match self {
            Self::OpenAi { effort, .. } | Self::Anthropic { effort, .. } => effort
                .as_ref()
                .map(|e| format!(":effort={e}"))
                .unwrap_or_default(),
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

    /// 覆盖单次调用的输出 token 上限；provider 在本次模型构造后即被消费。
    pub fn with_max_tokens(&self, max_tokens: u32) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::OpenAi {
                max_tokens: configured_max_tokens,
                ..
            }
            | Self::Anthropic {
                max_tokens: configured_max_tokens,
                ..
            } => *configured_max_tokens = max_tokens,
        }
        clone
    }

    /// 绑定 retry observer；`into_model()` 构造模型时注入 runtime。
    pub fn with_retry_observer(
        mut self,
        observer: Option<Arc<dyn peri_model::RetryObserver>>,
    ) -> Self {
        match &mut self {
            Self::OpenAi { retry_observer, .. } | Self::Anthropic { retry_observer, .. } => {
                *retry_observer = observer;
            }
        }
        self
    }

    /// 获取模型的上下文窗口大小（不消费 self）。
    ///
    /// 历史实现通过 `into_model().context_window()` 取值，OpenAI 与 Anthropic
    /// provider 均返回 200_000；`peri_model::Model` 不暴露 context_window，
    /// 此处保持配置级常量语义（1M 窗口由 `context_1m()` 标志在调用侧覆盖）。
    pub fn context_window(&self) -> u32 {
        200_000
    }

    pub fn into_model(self) -> Box<dyn peri_model::Model> {
        match self {
            Self::OpenAi {
                api_key,
                base_url,
                model,
                effort,
                max_tokens,
                retry_observer,
                ..
            } => {
                let endpoint =
                    parse_endpoint(&base_url, "https://api.openai.com/v1", "openai base_url");
                let mut config = OpenAiConfig::new(endpoint, api_key, model);
                if let Some(e) = effort.as_ref() {
                    config = config.with_reasoning_effort(e);
                    config = config.with_thinking_enabled(true);
                }
                config = config.with_max_tokens(max_tokens);
                // 全量观测：langfuse input 与实际发送请求体一致（敏感键/data URI 仍强制脱敏）
                config = config.with_runtime(match retry_observer {
                    Some(observer) => peri_model::ModelRuntimeConfig::with_full_observation()
                        .with_retry_observer(observer),
                    None => peri_model::ModelRuntimeConfig::with_full_observation(),
                });
                Box::new(OpenAiModel::new(config))
            }
            Self::Anthropic {
                api_key,
                model,
                base_url,
                effort,
                max_tokens,
                retry_observer,
                ..
            } => {
                let endpoint = match base_url {
                    Some(url) => {
                        parse_endpoint(&url, "https://api.anthropic.com", "anthropic base_url")
                    }
                    None => Url::parse("https://api.anthropic.com").expect("静态默认 endpoint"),
                };
                let output_max_tokens = max_tokens;
                let mut config = AnthropicConfig::new(endpoint, api_key, model);
                if let Some(e) = effort
                    .as_ref()
                    .filter(|_| output_max_tokens > MIN_ANTHROPIC_THINKING_BUDGET)
                {
                    let thinking_budget =
                        DEFAULT_ANTHROPIC_THINKING_BUDGET.min(output_max_tokens.saturating_sub(1));
                    config = config.with_extended_thinking(thinking_budget, e);
                }
                config = config.with_max_tokens(output_max_tokens);
                // 全量观测：langfuse input 与实际发送请求体一致（敏感键/data URI 仍强制脱敏）
                config = config.with_runtime(match retry_observer {
                    Some(observer) => peri_model::ModelRuntimeConfig::with_full_observation()
                        .with_retry_observer(observer),
                    None => peri_model::ModelRuntimeConfig::with_full_observation(),
                });
                Box::new(AnthropicModel::new(config))
            }
        }
    }
}

/// 解析 active profile → (provider, profile)。
/// profile.provider 为空时回退第一个可用 provider；provider 找不到返回 None。
fn resolve_profile<'a>(
    app: &'a config::AppConfig,
    alias: &str,
) -> Option<(&'a ProviderConfig, &'a config::ProfileConfig)> {
    let profile = app.profiles.get(alias)?;
    let provider = if profile.provider.is_empty() {
        app.providers.first()
    } else {
        app.providers.iter().find(|p| p.id == profile.provider)
    }?;
    Some((provider, profile))
}

/// 解析最终 model 名：Profile.model > ProviderModels 同档位（fable 空回退 opus）> 厂商默认
fn resolve_model_name(
    provider: &ProviderConfig,
    alias: &str,
    profile: &config::ProfileConfig,
) -> String {
    if let Some(m) = profile.model.as_ref().filter(|m| !m.is_empty()) {
        return m.clone();
    }
    provider
        .models
        .get_model(alias)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match provider.provider_type.as_str() {
            "anthropic" => "claude-sonnet-4-6".to_string(),
            _ => "gpt-4o".to_string(),
        })
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
