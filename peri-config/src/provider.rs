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

impl From<EnvironmentProvider> for ResolvedProvider {
    fn from(provider: EnvironmentProvider) -> Self {
        match provider {
            EnvironmentProvider::Anthropic {
                api_key,
                model,
                base_url,
            } => Self::Anthropic {
                api_key,
                model,
                base_url,
                effort: None,
                max_tokens: 32000,
                context_1m: false,
            },
            EnvironmentProvider::OpenAi {
                api_key,
                model,
                base_url,
            } => Self::OpenAi {
                api_key,
                model,
                base_url,
                effort: None,
                max_tokens: 32000,
                context_1m: false,
            },
        }
    }
}

pub fn resolve(
    settings: &PeriConfig,
    environment: &BTreeMap<String, String>,
) -> Option<ResolvedProvider> {
    resolve_for_alias(settings, &settings.config.active_alias)
        .or_else(|| EnvironmentProvider::resolve(environment).map(ResolvedProvider::from))
}

pub fn resolve_for_alias(settings: &PeriConfig, alias: &str) -> Option<ResolvedProvider> {
    let (provider, profile) = resolve_profile(&settings.config, alias)?;
    if provider.api_key.is_empty() {
        return None;
    }
    let model = resolve_model_name(provider, alias, profile);
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
        .unwrap_or_else(|| match provider.provider_type.as_str() {
            "anthropic" => DEFAULT_ANTHROPIC_MODEL.to_owned(),
            _ => DEFAULT_OPENAI_MODEL.to_owned(),
        })
}

pub const ENVIRONMENT_KEYS: &[&str] = &[
    "MODEL_PROVIDER",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "OPENAI_API_BASE",
    "OPENAI_BASE_URL",
    "OPENAI_MODEL",
];

#[derive(Clone, PartialEq, Eq)]
pub enum EnvironmentProvider {
    Anthropic {
        api_key: String,
        model: String,
        base_url: Option<String>,
    },
    OpenAi {
        api_key: String,
        base_url: String,
        model: String,
    },
}

impl fmt::Debug for EnvironmentProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anthropic {
                model, base_url, ..
            } => formatter
                .debug_struct("Anthropic")
                .field("api_key", &"[REDACTED]")
                .field("model", model)
                .field("base_url", base_url)
                .finish(),
            Self::OpenAi {
                base_url, model, ..
            } => formatter
                .debug_struct("OpenAi")
                .field("api_key", &"[REDACTED]")
                .field("base_url", base_url)
                .field("model", model)
                .finish(),
        }
    }
}

impl EnvironmentProvider {
    pub fn resolve(environment: &BTreeMap<String, String>) -> Option<Self> {
        let provider_hint = environment
            .get("MODEL_PROVIDER")
            .map(String::as_str)
            .unwrap_or_default();
        let normalized_hint = provider_hint.to_lowercase();

        if normalized_hint == "anthropic"
            || (normalized_hint.is_empty() && environment.contains_key("ANTHROPIC_API_KEY"))
        {
            let api_key = environment.get("ANTHROPIC_API_KEY")?.clone();
            return Some(Self::Anthropic {
                api_key,
                model: environment
                    .get("ANTHROPIC_MODEL")
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.to_owned()),
                base_url: environment.get("ANTHROPIC_BASE_URL").cloned(),
            });
        }

        let api_key = environment.get("OPENAI_API_KEY")?.clone();
        Some(Self::OpenAi {
            api_key,
            base_url: environment
                .get("OPENAI_API_BASE")
                .or_else(|| environment.get("OPENAI_BASE_URL"))
                .cloned()
                .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_owned()),
            model: environment
                .get("OPENAI_MODEL")
                .cloned()
                .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_owned()),
        })
    }
}

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
    fn explicit_anthropic_uses_defaults_and_optional_endpoint() {
        assert_eq!(
            EnvironmentProvider::resolve(&env(&[
                ("MODEL_PROVIDER", "Anthropic"),
                ("ANTHROPIC_API_KEY", "secret"),
            ])),
            Some(EnvironmentProvider::Anthropic {
                api_key: "secret".into(),
                model: "claude-sonnet-4-6".into(),
                base_url: None,
            })
        );
    }

    #[test]
    fn empty_provider_hint_prefers_anthropic_key() {
        assert!(matches!(
            EnvironmentProvider::resolve(&env(&[
                ("ANTHROPIC_API_KEY", "secret"),
                ("OPENAI_API_KEY", "other"),
            ])),
            Some(EnvironmentProvider::Anthropic { .. })
        ));
    }

    #[test]
    fn openai_api_base_precedes_base_url_even_when_empty() {
        assert_eq!(
            EnvironmentProvider::resolve(&env(&[
                ("MODEL_PROVIDER", "openai"),
                ("OPENAI_API_KEY", "secret"),
                ("OPENAI_API_BASE", ""),
                ("OPENAI_BASE_URL", "https://fallback.example"),
            ])),
            Some(EnvironmentProvider::OpenAi {
                api_key: "secret".into(),
                base_url: String::new(),
                model: "gpt-4o".into(),
            })
        );
    }

    #[test]
    fn unknown_provider_hint_falls_back_to_openai() {
        assert!(matches!(
            EnvironmentProvider::resolve(&env(&[
                ("MODEL_PROVIDER", "custom"),
                ("OPENAI_API_KEY", "secret"),
            ])),
            Some(EnvironmentProvider::OpenAi { .. })
        ));
    }

    #[test]
    fn missing_required_api_key_returns_none() {
        assert_eq!(EnvironmentProvider::resolve(&BTreeMap::new()), None);
        assert_eq!(
            EnvironmentProvider::resolve(&env(&[("MODEL_PROVIDER", "anthropic")])),
            None
        );
    }

    #[test]
    fn debug_redacts_api_key() {
        let provider =
            EnvironmentProvider::resolve(&env(&[("OPENAI_API_KEY", "private-key")])).unwrap();
        let debug = format!("{provider:?}");
        assert!(!debug.contains("private-key"));
        assert!(debug.contains("[REDACTED]"));
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
    fn settings_take_priority_and_profile_parameters_are_preserved() {
        let mut config = settings("openai");
        config.config.profiles.opus = ProfileConfig {
            provider: "configured".into(),
            model: Some("selected-model".into()),
            effort: "max".into(),
            max_tokens: 64000,
            context_1m: true,
        };
        assert_eq!(
            resolve(&config, &env(&[("ANTHROPIC_API_KEY", "environment-key")])),
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
    fn invalid_settings_fall_back_to_environment_with_legacy_parameters() {
        let environment = env(&[("OPENAI_API_KEY", "environment-key")]);
        let mut config = settings("openai");
        config.config.providers[0].api_key.clear();
        assert_eq!(resolve_for_alias(&config, "opus"), None);
        assert_eq!(
            resolve(&config, &environment),
            Some(ResolvedProvider::OpenAi {
                api_key: "environment-key".into(),
                base_url: DEFAULT_OPENAI_BASE_URL.into(),
                model: DEFAULT_OPENAI_MODEL.into(),
                effort: None,
                max_tokens: 32000,
                context_1m: false,
            })
        );
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
