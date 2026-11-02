//! LLM Provider and model configuration.
//!
//! Manages provider configuration, model alias resolution, and LLM factory creation.
//! Decoupled from TUI-specific types.

pub mod auth;
pub mod config;
pub mod store;

use std::sync::Arc;

use peri_config::provider::{ResolvedProvider, ResponsesAuthentication};

pub use config::{AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels};
use peri_model::{
    AnthropicConfig, AnthropicModel, OpenAiConfig, OpenAiModel, ResponsesConfig, ResponsesModel,
    ResponsesProfile,
};
pub use store::{
    config_path, load, load_from, save_to, set_global_config_path, workspace_config_path,
    ConfigSource,
};
use url::Url;

const DEFAULT_ANTHROPIC_THINKING_BUDGET: u32 = 10_000;

#[derive(Clone)]
pub enum LlmProvider {
    Responses {
        auth: ResponsesAuthentication,
        endpoint: Url,
        model: String,
        effort: Option<String>,
        max_tokens: u32,
        context_window: u32,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    },
    /// OpenAI 兼容 Provider。`base_url` 需要 `/v1` 后缀。
    OpenAi {
        api_key: String,
        base_url: String,
        model: String,
        /// 思考强度 "low".."max"；None 表示不启用 extended thinking
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        context_window: u32,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    },
    Anthropic {
        api_key: String,
        model: String,
        base_url: Option<String>,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        context_window: u32,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    },
}

impl LlmProvider {
    pub fn from_source(source: &peri_config::settings::ConfigSource) -> Option<Self> {
        if let Some(snapshot) = source.snapshot() {
            return snapshot.provider().cloned().map(Self::from_resolved);
        }
        let environment =
            peri_config::source::read_environment(peri_config::provider::ENVIRONMENT_KEYS).ok()?;
        peri_config::provider::resolve(&source.loaded_merged(), &environment)
            .map(Self::from_resolved)
    }

    pub fn from_resolved(provider: ResolvedProvider) -> Self {
        match provider {
            ResolvedProvider::Responses {
                auth,
                endpoint,
                model,
                effort,
                max_tokens,
                context_window,
            } => Self::Responses {
                auth,
                endpoint,
                model,
                effort,
                max_tokens,
                context_window,
                retry_observer: None,
            },
            ResolvedProvider::Anthropic {
                api_key,
                model,
                base_url,
                effort,
                max_tokens,
                context_1m,
                context_window,
            } => Self::Anthropic {
                api_key,
                model,
                base_url,
                effort,
                max_tokens,
                context_1m,
                context_window,
                retry_observer: None,
            },
            ResolvedProvider::OpenAi {
                api_key,
                base_url,
                model,
                effort,
                max_tokens,
                context_1m,
                context_window,
            } => Self::OpenAi {
                api_key,
                base_url,
                model,
                effort,
                max_tokens,
                context_1m,
                context_window,
                retry_observer: None,
            },
        }
    }

    /// 从当前配置和本地配置环境构造 Provider；初始 scoped 配置优先用 `from_source`。
    pub fn from_config(cfg: &config::PeriConfig) -> Option<Self> {
        let environment =
            peri_config::source::read_environment(peri_config::provider::ENVIRONMENT_KEYS).ok()?;
        peri_config::provider::resolve(cfg, &environment).map(Self::from_resolved)
    }

    /// 从 PeriConfig 按指定档位（"fable"/"opus"/"sonnet"/"haiku"）构造 LlmProvider。
    /// Profile 是唯一事实源：provider/model/effort/max_tokens/窗口配置全部取自
    /// `profiles[alias]`，model 空时回退 provider.models 同档位映射（fable 空回退 opus）。
    pub fn from_config_for_alias(cfg: &config::PeriConfig, alias: &str) -> Option<Self> {
        peri_config::provider::resolve_for_alias(cfg, alias).map(Self::from_resolved)
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::Responses {
                auth: ResponsesAuthentication::ChatGptPlan { .. },
                ..
            } => "ChatGPT plan (Responses)",
            Self::Responses { .. } => "OpenAI Responses",
            Self::OpenAi { .. } => "OpenAI",
            Self::Anthropic { .. } => "Anthropic",
        }
    }

    pub fn model_name(&self) -> &str {
        match self {
            Self::Responses { model, .. } | Self::OpenAi { model, .. } => model,
            Self::Anthropic { model, .. } => model,
        }
    }

    pub fn context_1m(&self) -> bool {
        match self {
            Self::Responses { .. } => false,
            Self::OpenAi { context_1m, .. } | Self::Anthropic { context_1m, .. } => *context_1m,
        }
    }

    /// 思考强度稳定标识，用于 fingerprint；None 时返回空字符串
    pub fn effort_key(&self) -> String {
        match self {
            Self::Responses { effort, .. }
            | Self::OpenAi { effort, .. }
            | Self::Anthropic { effort, .. } => effort
                .as_ref()
                .map(|e| format!(":effort={e}"))
                .unwrap_or_default(),
        }
    }

    /// 替换模型名，保持其他配置不变
    pub fn with_model_name(&self, model: String) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::Responses { model: m, .. } | Self::OpenAi { model: m, .. } => *m = model,
            Self::Anthropic { model: m, .. } => *m = model,
        }
        clone
    }

    /// 覆盖单次调用的输出 token 上限；provider 在本次模型构造后即被消费。
    /// ChatGPT plan 不支持服务器输出上限，Responses adapter 在该模式忽略此值。
    pub fn with_max_tokens(&self, max_tokens: u32) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::Responses {
                max_tokens: configured_max_tokens,
                ..
            }
            | Self::OpenAi {
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
            Self::Responses { retry_observer, .. }
            | Self::OpenAi { retry_observer, .. }
            | Self::Anthropic { retry_observer, .. } => {
                *retry_observer = observer;
            }
        }
        self
    }

    /// 配置权威面已解析的有效容量；模型 metadata 与宿主预算消费相同值。
    pub fn context_window(&self) -> u32 {
        match self {
            Self::Responses { context_window, .. }
            | Self::OpenAi { context_window, .. }
            | Self::Anthropic { context_window, .. } => *context_window,
        }
    }

    pub fn effective_context_window(&self) -> u32 {
        self.context_window()
    }

    pub fn into_model(self) -> Box<dyn peri_model::Model> {
        let context_window = self.effective_context_window();
        match self {
            Self::Responses {
                auth,
                endpoint,
                model,
                effort,
                max_tokens,
                retry_observer,
                ..
            } => {
                let mut config = match auth {
                    ResponsesAuthentication::ApiKey { api_key } => {
                        let credential_ref = credential_identity(&api_key);
                        ResponsesConfig::new(endpoint, api_key, model)
                            .with_profile(ResponsesProfile::Platform)
                            .with_credential_ref(credential_ref)
                    }
                    ResponsesAuthentication::ChatGptPlan {
                        store,
                        account,
                        subject,
                        client_id,
                    } => {
                        let auth = auth::request_auth(store, account, subject, client_id);
                        let credential_ref = auth.credential_ref().to_owned();
                        ResponsesConfig::new(endpoint, "", model)
                            .with_profile(ResponsesProfile::ChatGptPlan)
                            .with_credential_ref(credential_ref)
                            .with_auth(auth)
                    }
                };
                if let Some(effort) = effort {
                    config = config.with_reasoning_effort(effort);
                }
                config = config
                    .with_max_tokens(max_tokens)
                    .with_context_window(Some(context_window))
                    .with_runtime(match retry_observer {
                        Some(observer) => peri_model::ModelRuntimeConfig::with_full_observation()
                            .with_retry_observer(observer),
                        None => peri_model::ModelRuntimeConfig::with_full_observation(),
                    });
                Box::new(ResponsesModel::new(config))
            }
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
                config = config.with_context_window(Some(context_window));
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
                // 模式与预算有效性由 Anthropic adapter 决定；adaptive 不受手动预算门限限制。
                if let Some(e) = &effort {
                    let thinking_budget =
                        DEFAULT_ANTHROPIC_THINKING_BUDGET.min(output_max_tokens.saturating_sub(1));
                    config = config.with_extended_thinking(thinking_budget, e);
                }
                config = config.with_max_tokens(output_max_tokens);
                config = config.with_context_window(Some(context_window));
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

#[cfg(test)]
#[path = "responses_test.rs"]
mod responses_tests;

// Replay bindings use a digest; bearer credentials and local store paths never
// become provider-state fields in a saved transcript.
fn credential_identity(value: &impl serde::Serialize) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let bytes = serde_json::to_vec(value).expect("credential identity is serializable");
    let mut identity = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(identity, "{byte:02x}");
    }
    identity
}
