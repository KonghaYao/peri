use super::*;
use serde_json::json;

fn settings() -> PeriConfig {
    serde_json::from_value(json!({"config": {
        "active_alias": "opus",
        "providers": [{"id":"responses", "type":"openai_responses", "apiKey":"fixture-key",
            "models":{"opus":"main-model", "haiku":"small-model"}}],
        "profiles": {
            "opus":{"provider":"responses", "effort":"high", "max_tokens":4096, "contextWindow":120000},
            "haiku":{"provider":"responses", "effort":"low", "max_tokens":2048, "contextWindow":64000}
        }
    }})).unwrap()
}

#[test]
fn test_responses_profile_selection_owns_model_capacity_and_parameters() {
    let settings = settings();
    let Some(ResolvedProvider::Responses {
        model,
        effort,
        context_window,
        max_tokens,
        endpoint,
        ..
    }) = try_resolve_for_alias(&settings, "haiku").unwrap()
    else {
        panic!("应选择 Responses");
    };
    assert_eq!(model, "small-model");
    assert_eq!(effort.as_deref(), Some("low"));
    assert_eq!((context_window, max_tokens), (64000, 2048));
    assert_eq!(endpoint.as_str(), DEFAULT_RESPONSES_URL);
}

#[test]
fn test_subscription_reference_roundtrips_without_api_key() {
    let mut settings = settings();
    let provider = &mut settings.config.providers[0];
    provider.api_key.clear();
    provider.auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: "/tmp/peri-test-credentials".into(),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    let encoded = serde_json::to_value(&settings).unwrap();
    assert_eq!(
        encoded["config"]["providers"][0]["auth"]["kind"],
        "chatgpt_plan"
    );
    let decoded: PeriConfig = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, settings);
    assert!(
        matches!(try_resolve(&decoded, &BTreeMap::new()).unwrap(), Some(
        ResolvedProvider::Responses {auth: ResponsesAuthentication::ChatGptPlan {account, ..}, ..}
    ) if account == "work")
    );
}

#[test]
fn test_subscription_never_falls_back_to_configured_api_key() {
    let mut settings = settings();
    settings.config.providers[0].auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: "/tmp/peri-test-credentials".into(),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::InvalidSubscriptionReference)
    ));
}

#[test]
fn test_responses_requires_explicit_capacity_even_with_legacy_1m_flag() {
    let mut settings = settings();
    settings.config.profiles.opus.context_window = None;
    settings.config.profiles.opus.context_1m = true;
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::MissingModelCapacity)
    ));
}

#[test]
fn test_unknown_provider_type_does_not_select_chat_completions() {
    let mut settings = settings();
    settings.config.providers[0].provider_type = "openai_reponses".into();
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::UnknownType)
    ));
    assert_eq!(resolve(&settings, &BTreeMap::new()), None);
}

#[test]
fn test_bad_endpoint_does_not_fall_back_to_public_api() {
    let mut settings = settings();
    settings.config.providers[0].base_url = "not-a-url".into();
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::InvalidEndpoint)
    ));
}

#[test]
fn test_subscription_cannot_send_credentials_to_custom_endpoint() {
    let mut settings = settings();
    let provider = &mut settings.config.providers[0];
    provider.api_key.clear();
    provider.auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: "/tmp/peri-test-credentials".into(),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    provider.base_url = "https://example.invalid/v1/responses".into();
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::InvalidSubscriptionEndpoint)
    ));
}

#[test]
fn test_auth_rejects_untyped_token_fields() {
    let error = serde_json::from_value::<ProviderAuthConfig>(json!({
        "kind":"chatgpt_plan", "store":"/tmp/test", "account":"work", "access_token":"fixture"
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn test_subscription_alias_without_verified_identity_is_rejected() {
    let error = serde_json::from_value::<ProviderAuthConfig>(json!({
        "kind":"chatgpt_plan", "store":"/tmp/test", "account":"work"
    }))
    .unwrap_err();
    assert!(error.to_string().contains("missing field"));
}

/// 辅助档位缺少 Responses 模型/窗口属于未完成编辑：不阻断快照解析，也不影响
/// 其它配置保存；当前生效档位仍然必须可解析。
#[test]
fn test_auxiliary_profile_missing_capacity_does_not_reject_snapshot_resolution() {
    let mut settings = settings();
    settings.config.profiles.haiku.context_window = None;
    let resolved = try_resolve(&settings, &BTreeMap::new()).unwrap();
    assert!(resolved.is_some(), "当前生效档位仍应解析成功");

    settings.config.profiles.opus.context_window = None;
    settings.config.active_alias = "opus".into();
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::MissingModelCapacity)
    ));
}

/// provider 级错误（缺少 API key）仍然拒绝整个快照，避免坏 provider
/// 成为隐式回退。
#[test]
fn test_auxiliary_provider_level_error_still_rejects_snapshot_resolution() {
    let mut settings = settings();
    settings.config.providers.push(ProviderConfig {
        id: "other".into(),
        provider_type: "anthropic".into(),
        api_key: "other-key".into(),
        ..Default::default()
    });
    // active 档位走另一个 provider；坏的是辅助档位绑定的 Responses provider。
    settings.config.profiles.opus.provider = "other".into();
    settings.config.providers[0].api_key.clear();
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::MissingApiKey)
    ));
}

#[test]
fn test_missing_responses_api_key_is_an_error_not_a_factory_fallback() {
    let mut settings = settings();
    settings.config.providers[0].api_key.clear();
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::MissingApiKey)
    ));
}

#[test]
fn provider_level_responses_validation_is_reusable_without_profiles() {
    let settings = settings();
    let access = validate_responses_provider(&settings.config.providers[0]).unwrap();
    assert_eq!(access.endpoint.as_str(), DEFAULT_RESPONSES_URL);
    assert_eq!(
        access.auth,
        ResponsesAuthentication::ApiKey {
            api_key: "fixture-key".into()
        }
    );

    let mut missing_key = settings.config.providers[0].clone();
    missing_key.api_key.clear();
    assert!(matches!(
        validate_responses_provider(&missing_key),
        Err(ProviderError::MissingApiKey)
    ));

    let mut bad_endpoint = settings.config.providers[0].clone();
    bad_endpoint.base_url = "not-a-url".into();
    assert!(matches!(
        validate_responses_provider(&bad_endpoint),
        Err(ProviderError::InvalidEndpoint)
    ));
}

#[test]
fn provider_level_subscription_validation_matches_resolver_contract() {
    let settings = settings();
    let mut provider = settings.config.providers[0].clone();
    provider.api_key.clear();
    provider.auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: "/tmp/peri-test-credentials".into(),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    let access = validate_responses_provider(&provider).unwrap();
    assert!(matches!(
        access.auth,
        ResponsesAuthentication::ChatGptPlan { .. }
    ));

    let mut relative_store = provider.clone();
    if let Some(ProviderAuthConfig::ChatGptPlan { store, .. }) = &mut relative_store.auth {
        *store = "relative/store".into();
    }
    assert!(matches!(
        validate_responses_provider(&relative_store),
        Err(ProviderError::InvalidSubscriptionReference)
    ));

    let mut custom_endpoint = provider;
    custom_endpoint.base_url = "https://example.invalid/v1/responses".into();
    assert!(matches!(
        validate_responses_provider(&custom_endpoint),
        Err(ProviderError::InvalidSubscriptionEndpoint)
    ));
}

#[test]
fn provider_level_validation_does_not_change_profile_capacity_contract() {
    let mut settings = settings();
    let access = validate_responses_provider(&settings.config.providers[0]).unwrap();
    assert_eq!(access.endpoint.as_str(), DEFAULT_RESPONSES_URL);

    // Provider-level validation intentionally ignores profiles; the resolver still owns
    // the profile capacity requirement.
    settings.config.profiles.opus.context_window = Some(0);
    assert!(
        validate_responses_provider(&settings.config.providers[0]).is_ok(),
        "provider-level validation has no profile input"
    );
    assert!(matches!(
        try_resolve_for_alias(&settings, "opus"),
        Err(ProviderError::MissingModelCapacity)
    ));
}
