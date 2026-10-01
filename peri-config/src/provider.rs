use std::collections::BTreeMap;
use std::fmt;

use crate::app::{AppConfig, PeriConfig, ProfileConfig, ProviderConfig};

const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_OPENAI_MODEL: &str = "gpt-4o";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedProvider {
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

impl fmt::Debug for ResolvedProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Anthropic { .. } => "Anthropic",
            Self::OpenAi { .. } => "OpenAi",
        };
        formatter
            .debug_struct(name)
            .field("configuration", &"[REDACTED]")
            .finish()
    }
}

pub fn resolve(
    settings: &PeriConfig,
    environment: &BTreeMap<String, String>,
) -> Option<ResolvedProvider> {
    match (
        environment.get("MODEL_PROVIDER"),
        environment.get("MODEL_TYPE"),
    ) {
        (None, None) => resolve_for_alias(settings, &settings.config.active_alias),
        (Some(provider_id), Some(alias)) => {
            resolve_for_provider_alias(settings, provider_id, alias)
        }
        _ => None,
    }
}

pub fn resolve_for_alias(settings: &PeriConfig, alias: &str) -> Option<ResolvedProvider> {
    let (provider, profile) = resolve_profile(&settings.config, alias)?;
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
) -> Option<ResolvedProvider> {
    let provider = settings
        .config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id && !provider_id.is_empty())?;
    let profile = settings.config.profiles.get(alias)?;
    let model = provider
        .models
        .get_model(alias)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)?;
    resolve_configured(provider, profile, model)
}

fn resolve_configured(
    provider: &ProviderConfig,
    profile: &ProfileConfig,
    model: String,
) -> Option<ResolvedProvider> {
    if provider.api_key.is_empty() {
        return None;
    }
    let effort = Some(profile.effort.clone());
    let max_tokens = profile.max_tokens;
    let context_1m = profile.context_1m;
    match provider.provider_type.as_str() {
        "anthropic" => Some(ResolvedProvider::Anthropic {
            api_key: provider.api_key.clone(),
            model,
            base_url: (!provider.base_url.is_empty()).then(|| provider.base_url.clone()),
            effort,
            max_tokens,
            context_1m,
        }),
        _ => Some(ResolvedProvider::OpenAi {
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
        }),
    }
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
    } else {
        DEFAULT_OPENAI_MODEL
    }
}

pub const ENVIRONMENT_KEYS: &[&str] = &["MODEL_PROVIDER", "MODEL_TYPE"];

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
        config.config.providers[0].provider_type = "custom".into();
        config.config.providers[0].base_url = "https://configured.example/v1".into();
        assert!(
            matches!(resolve_for_alias(&config, "fable"), Some(ResolvedProvider::OpenAi { model, base_url, .. }) if model == DEFAULT_OPENAI_MODEL && base_url == "https://configured.example/v1")
        );
    }

    #[test]
    fn resolved_debug_redacts_both_variants() {
        for provider_type in ["anthropic", "openai"] {
            let mut config = settings(provider_type);
            config.config.providers[0].base_url = "https://private-key.example".into();
            let provider = resolve_for_alias(&config, "opus").unwrap();
            let debug = format!("{provider:?}");
            assert!(!debug.contains("configured-key"));
            assert!(!debug.contains("private-key"));
            assert!(debug.contains("[REDACTED]"));
        }
    }
}
