use std::collections::BTreeMap;

use crate::app::{AppConfig, PeriConfig, ProfileConfig, ProviderAuthConfig, ProviderConfig};
use thiserror::Error;
use url::Url;

const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";
const DEFAULT_OPENAI_MODEL: &str = "gpt-6.1-sol";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponsesAuthentication {
    ApiKey {
        api_key: String,
    },
    ChatGptPlan {
        store: std::path::PathBuf,
        account: String,
        subject: String,
        client_id: String,
    },
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("unsupported provider type; use anthropic, openai, or openai_responses")]
    UnknownType,
    #[error("Responses requires an explicit model and positive profile contextWindow")]
    MissingModelCapacity,
    #[error(
        "Responses baseUrl must be an absolute HTTP(S) URL without user info, query, or fragment"
    )]
    InvalidEndpoint,
    #[error("ChatGPT plan requires an absolute credentials store and nonempty account/subject/clientId reference, with no apiKey")]
    InvalidSubscriptionReference,
    #[error("ChatGPT plan requires the official https://api.openai.com/v1/responses endpoint")]
    InvalidSubscriptionEndpoint,
    #[error("auth configuration is only supported by openai_responses")]
    UnexpectedAuth,
    #[error("Responses API key mode requires apiKey; choose chatgpt_plan explicitly for subscription authentication")]
    MissingApiKey,
}

/// Provider-level Responses endpoint and authentication, validated independently of any
/// profile.
///
/// This is the single implementation shared by [`resolve_responses`] and configuration
/// editors: a Responses provider must be valid even before it owns a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesProviderAccess {
    pub endpoint: Url,
    pub auth: ResponsesAuthentication,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedProvider {
    Responses {
        auth: ResponsesAuthentication,
        endpoint: Url,
        model: String,
        effort: Option<String>,
        max_tokens: u32,
        context_window: u32,
    },
    Anthropic {
        api_key: String,
        model: String,
        base_url: Option<String>,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
    },
    OpenAi {
        api_key: String,
        model: String,
        base_url: String,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
    },
}

pub fn resolve(
    settings: &PeriConfig,
    environment: &BTreeMap<String, String>,
) -> Option<ResolvedProvider> {
    try_resolve(settings, environment).unwrap_or_else(|error| {
        tracing::warn!(%error, "provider configuration rejected");
        None
    })
}

pub fn try_resolve(
    settings: &PeriConfig,
    environment: &BTreeMap<String, String>,
) -> Result<Option<ResolvedProvider>, ProviderError> {
    // Validate configured auxiliary tiers at the same authority boundary. An invalid
    // subscription tier must not later become a factory's implicit primary fallback.
    for alias in crate::app::Profiles::ALL {
        if settings
            .config
            .profiles
            .get(alias)
            .is_some_and(|profile| !profile.is_default())
        {
            try_resolve_for_alias(settings, alias)?;
        }
    }
    match (
        environment.get("MODEL_PROVIDER"),
        environment.get("MODEL_TYPE"),
    ) {
        (None, None) => try_resolve_for_alias(settings, &settings.config.active_alias),
        (Some(provider_id), Some(alias)) => {
            resolve_for_provider_alias(settings, provider_id, alias)
        }
        _ => Ok(None),
    }
}

pub fn resolve_for_alias(settings: &PeriConfig, alias: &str) -> Option<ResolvedProvider> {
    try_resolve_for_alias(settings, alias).unwrap_or_else(|error| {
        tracing::warn!(%error, "provider profile rejected");
        None
    })
}

pub fn try_resolve_for_alias(
    settings: &PeriConfig,
    alias: &str,
) -> Result<Option<ResolvedProvider>, ProviderError> {
    let Some((provider, profile)) = resolve_profile(&settings.config, alias) else {
        return Ok(None);
    };
    resolve_configured(
        provider,
        profile,
        resolve_model_name(provider, alias, profile),
    )
}

/// Explicit environment selection binds a configured provider ID and a model tier.
/// The selected provider's model mapping owns the model name; a profile.model bound
/// to another provider must not silently replace it.
fn resolve_for_provider_alias(
    settings: &PeriConfig,
    provider_id: &str,
    alias: &str,
) -> Result<Option<ResolvedProvider>, ProviderError> {
    let Some(provider) = settings
        .config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id && !provider_id.is_empty())
    else {
        return Ok(None);
    };
    let Some(profile) = settings.config.profiles.get(alias) else {
        return Ok(None);
    };
    let Some(model) = provider
        .models
        .get_model(alias)
        .filter(|model| !model.is_empty())
    else {
        return Ok(None);
    };
    resolve_configured(provider, profile, model.to_owned())
}

fn resolve_configured(
    provider: &ProviderConfig,
    profile: &ProfileConfig,
    model: String,
) -> Result<Option<ResolvedProvider>, ProviderError> {
    if provider.provider_type == "openai_responses" {
        return resolve_responses(provider, profile, model);
    }
    if provider.auth.is_some() {
        return Err(ProviderError::UnexpectedAuth);
    }
    if !matches!(provider.provider_type.as_str(), "anthropic" | "openai" | "") {
        return Err(ProviderError::UnknownType);
    }
    if provider.api_key.is_empty() {
        return Ok(None);
    }
    let effort = Some(profile.effort.clone());
    let max_tokens = profile.max_tokens;
    let context_1m = profile.context_1m;
    Ok(Some(match provider.provider_type.as_str() {
        "anthropic" => ResolvedProvider::Anthropic {
            api_key: provider.api_key.clone(),
            model,
            base_url: (!provider.base_url.is_empty()).then(|| provider.base_url.clone()),
            effort,
            max_tokens,
            context_1m,
        },
        "openai" | "" => ResolvedProvider::OpenAi {
            api_key: provider.api_key.clone(),
            model,
            base_url: if provider.base_url.is_empty() {
                DEFAULT_OPENAI_BASE_URL.to_owned()
            } else {
                provider.base_url.clone()
            },
            effort,
            max_tokens,
            context_1m,
        },
        _ => return Err(ProviderError::UnknownType),
    }))
}

fn resolve_responses(
    provider: &ProviderConfig,
    profile: &ProfileConfig,
    model: String,
) -> Result<Option<ResolvedProvider>, ProviderError> {
    let context_window = profile
        .context_window
        .filter(|value| *value > 0)
        .filter(|_| !model.trim().is_empty())
        .ok_or(ProviderError::MissingModelCapacity)?;
    let ResponsesProviderAccess { endpoint, auth } = validate_responses_provider(provider)?;
    Ok(Some(ResolvedProvider::Responses {
        auth,
        endpoint,
        model,
        effort: (!profile.effort.is_empty()).then(|| profile.effort.clone()),
        max_tokens: profile.max_tokens,
        context_window,
    }))
}

/// Validate the endpoint and authentication of a Responses provider without consulting a
/// profile.
///
/// Editors use this to locate provider-field errors before any profile is bound; the
/// resolver calls it at the same authority boundary so both paths accept exactly the same
/// configurations. Profile model/capacity remains the caller's responsibility.
pub fn validate_responses_provider(
    provider: &ProviderConfig,
) -> Result<ResponsesProviderAccess, ProviderError> {
    let endpoint = Url::parse(if provider.base_url.is_empty() {
        DEFAULT_RESPONSES_URL
    } else {
        &provider.base_url
    })
    .map_err(|_| ProviderError::InvalidEndpoint)?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(ProviderError::InvalidEndpoint);
    }
    let auth = match &provider.auth {
        Some(ProviderAuthConfig::ChatGptPlan {
            store,
            account,
            subject,
            client_id,
        }) => {
            if !provider.api_key.is_empty()
                || !store.is_absolute()
                || account.trim().is_empty()
                || subject.trim().is_empty()
                || client_id.trim().is_empty()
            {
                return Err(ProviderError::InvalidSubscriptionReference);
            }
            if endpoint.as_str() != DEFAULT_RESPONSES_URL {
                return Err(ProviderError::InvalidSubscriptionEndpoint);
            }
            ResponsesAuthentication::ChatGptPlan {
                store: store.clone(),
                account: account.clone(),
                subject: subject.clone(),
                client_id: client_id.clone(),
            }
        }
        Some(ProviderAuthConfig::ApiKey) | None => {
            if provider.api_key.is_empty() {
                return Err(ProviderError::MissingApiKey);
            }
            ResponsesAuthentication::ApiKey {
                api_key: provider.api_key.clone(),
            }
        }
    };
    Ok(ResponsesProviderAccess { endpoint, auth })
}

fn resolve_profile<'a>(
    app: &'a AppConfig,
    alias: &str,
) -> Option<(&'a ProviderConfig, &'a ProfileConfig)> {
    let profile = app.profiles.get(alias)?;
    let provider = if profile.provider.is_empty() {
        app.providers.first()
    } else {
        app.providers
            .iter()
            .find(|provider| provider.id == profile.provider)
    }?;
    Some((provider, profile))
}

fn resolve_model_name(provider: &ProviderConfig, alias: &str, profile: &ProfileConfig) -> String {
    if let Some(model) = profile.model.as_ref().filter(|model| !model.is_empty()) {
        return model.clone();
    }
    provider
        .models
        .get_model(alias)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| default_model_name(&provider.provider_type).to_owned())
}

fn default_model_name(provider_type: &str) -> &str {
    if provider_type == "anthropic" {
        DEFAULT_ANTHROPIC_MODEL
    } else if provider_type == "openai_responses" {
        ""
    } else {
        DEFAULT_OPENAI_MODEL
    }
}

pub const ENVIRONMENT_KEYS: &[&str] = &["MODEL_PROVIDER", "MODEL_TYPE"];

#[cfg(test)]
#[path = "provider_responses_test.rs"]
mod responses_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ProviderModels;
    use std::collections::BTreeMap;

    fn env(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn environment_keys_are_unique() {
        let mut keys = ENVIRONMENT_KEYS.to_vec();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ENVIRONMENT_KEYS.len());
    }

    #[test]
    fn environment_pair_selects_configured_provider_and_tier() {
        let mut config = settings("anthropic");
        config.config.providers.push(ProviderConfig {
            id: "second".into(),
            provider_type: "openai".into(),
            api_key: "second-key".into(),
            models: ProviderModels {
                sonnet: "second-sonnet".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        config.config.profiles.sonnet.model = Some("first-provider-model".into());
        assert_eq!(
            resolve(
                &config,
                &env(&[("MODEL_PROVIDER", "second"), ("MODEL_TYPE", "sonnet")])
            ),
            Some(ResolvedProvider::OpenAi {
                api_key: "second-key".into(),
                model: "second-sonnet".into(),
                base_url: DEFAULT_OPENAI_BASE_URL.into(),
                effort: Some("xhigh".into()),
                max_tokens: 32000,
                context_1m: false,
            })
        );
    }

    #[test]
    fn incomplete_or_invalid_selection_does_not_use_active_profile() {
        let config = settings("openai");
        for selection in [
            env(&[("MODEL_PROVIDER", "configured")]),
            env(&[("MODEL_TYPE", "opus")]),
            env(&[("MODEL_PROVIDER", "missing"), ("MODEL_TYPE", "opus")]),
            env(&[("MODEL_PROVIDER", "configured"), ("MODEL_TYPE", "bad")]),
            env(&[("MODEL_PROVIDER", "configured"), ("MODEL_TYPE", "sonnet")]),
        ] {
            assert_eq!(resolve(&config, &selection), None);
        }
    }

    #[test]
    fn legacy_vendor_variables_do_not_select_or_supply_credentials() {
        let mut config = settings("openai");
        config.config.providers[0].api_key.clear();
        assert_eq!(
            resolve(
                &config,
                &env(&[
                    ("OPENAI_API_KEY", "legacy"),
                    ("ANTHROPIC_API_KEY", "legacy")
                ])
            ),
            None
        );
    }

    #[test]
    fn configured_provider_is_required_for_environment_selection() {
        let mut config = settings("anthropic");
        config.config.providers[0].api_key.clear();
        assert_eq!(
            resolve(
                &config,
                &env(&[("MODEL_PROVIDER", "configured"), ("MODEL_TYPE", "opus")])
            ),
            None
        );
    }

    fn settings(provider_type: &str) -> PeriConfig {
        PeriConfig {
            config: AppConfig {
                active_alias: "opus".into(),
                providers: vec![ProviderConfig {
                    id: "configured".into(),
                    provider_type: provider_type.into(),
                    api_key: "configured-key".into(),
                    models: ProviderModels {
                        opus: "tier-model".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn active_profile_is_used_without_environment_selection() {
        let mut config = settings("openai");
        config.config.profiles.opus = ProfileConfig {
            provider: "configured".into(),
            model: Some("selected-model".into()),
            effort: "max".into(),
            max_tokens: 64000,
            context_1m: true,
            context_window: None,
        };
        assert_eq!(
            resolve(&config, &BTreeMap::new()),
            Some(ResolvedProvider::OpenAi {
                api_key: "configured-key".into(),
                base_url: DEFAULT_OPENAI_BASE_URL.into(),
                model: "selected-model".into(),
                effort: Some("max".into()),
                max_tokens: 64000,
                context_1m: true,
            })
        );
    }

    #[test]
    fn invalid_settings_do_not_fall_back_to_vendor_credentials() {
        let environment = env(&[("OPENAI_API_KEY", "environment-key")]);
        let mut config = settings("openai");
        config.config.providers[0].api_key.clear();
        assert_eq!(resolve_for_alias(&config, "opus"), None);
        assert_eq!(resolve(&config, &environment), None);
    }

    #[test]
    fn provider_binding_is_exact_and_empty_binding_uses_first_provider() {
        let mut config = settings("anthropic");
        assert!(matches!(
            resolve_for_alias(&config, "OPUS"),
            Some(ResolvedProvider::Anthropic { .. })
        ));
        config.config.profiles.opus.provider = "missing".into();
        assert_eq!(resolve_for_alias(&config, "opus"), None);
        assert_eq!(resolve_for_alias(&config, "unknown"), None);
        config.config.providers.push(ProviderConfig {
            id: "second".into(),
            api_key: "second-key".into(),
            ..Default::default()
        });
        config.config.profiles.opus.provider = "second".into();
        assert!(
            matches!(resolve_for_alias(&config, "opus"), Some(ResolvedProvider::OpenAi { api_key, .. }) if api_key == "second-key")
        );
    }

    #[test]
    fn model_resolution_preserves_fable_fallback_and_vendor_defaults() {
        let mut config = settings("anthropic");
        assert!(
            matches!(resolve_for_alias(&config, "fable"), Some(ResolvedProvider::Anthropic { model, base_url: None, .. }) if model == "tier-model")
        );
        config.config.profiles.fable.model = Some(String::new());
        config.config.providers[0].models.opus.clear();
        assert!(
            matches!(resolve_for_alias(&config, "fable"), Some(ResolvedProvider::Anthropic { model, .. }) if model == DEFAULT_ANTHROPIC_MODEL)
        );
        config.config.providers[0].provider_type = "openai".into();
        config.config.providers[0].base_url = "https://configured.example/v1".into();
        assert!(
            matches!(resolve_for_alias(&config, "fable"), Some(ResolvedProvider::OpenAi { model, base_url, .. }) if model == DEFAULT_OPENAI_MODEL && base_url == "https://configured.example/v1")
        );
    }

    #[test]
    fn resolved_debug_preserves_both_variants() {
        for provider_type in ["anthropic", "openai"] {
            let mut config = settings(provider_type);
            config.config.providers[0].base_url = "https://private-key.example".into();
            let provider = resolve_for_alias(&config, "opus").unwrap();
            let debug = format!("{provider:?}");
            assert!(debug.contains("configured-key"));
            assert!(debug.contains("private-key"));
            assert!(!debug.contains("[REDACTED]"));
        }
    }
}
