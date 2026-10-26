use super::*;
use serde_json::json;

fn subscription_store() -> std::path::PathBuf {
    std::env::temp_dir().join("peri-test-credentials")
}

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
        store: subscription_store(),
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
        store: subscription_store(),
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
        store: subscription_store(),
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

/// 持久化边界：生效档位缺 model/contextWindow 属于未完成绑定，不能让整份配置保存失败；
/// 运行时解析仍按既有契约严格拒绝（截图中「切到 Responses provider 后保存失败」的根因）。
#[test]
fn test_snapshot_resolution_tolerates_missing_capacity_on_effective_profile() {
    let mut settings = settings();
    settings.config.profiles.opus.context_window = None;
    assert!(matches!(
        try_resolve(&settings, &BTreeMap::new()),
        Err(ProviderError::MissingModelCapacity)
    ));
    assert_eq!(
        try_resolve_snapshot(&settings, &BTreeMap::new()).unwrap(),
        None,
        "未完成绑定只让 provider 不可用，不拒绝配置文档"
    );
}

/// 持久化边界：非生效档位缺容量不影响生效档位保存（截图场景在 workspace 保存路径的同一规则）。
#[test]
fn test_snapshot_resolution_tolerates_auxiliary_capacity_gap() {
    let mut settings = settings();
    settings.config.providers.push(ProviderConfig {
        id: "cursor".into(),
        provider_type: "openai".into(),
        api_key: "cursor-key".into(),
        models: crate::app::ProviderModels {
            opus: "grok-4.6".into(),
            ..Default::default()
        },
        ..Default::default()
    });
    settings.config.profiles.opus.provider = "cursor".into();
    settings.config.profiles.opus.model = Some("grok-4.6".into());
    settings.config.profiles.opus.context_window = None;
    settings.config.profiles.haiku.context_window = None;

    let resolved = try_resolve_snapshot(&settings, &BTreeMap::new()).unwrap();
    assert!(
        matches!(resolved, Some(ResolvedProvider::OpenAi { ref model, .. }) if model == "grok-4.6"),
        "生效档位必须仍按自身绑定解析：{resolved:?}"
    );
}

/// 持久化边界：环境选择指定了缺容量的档位时同样视为未完成绑定，不阻断保存。
#[test]
fn test_snapshot_resolution_tolerates_capacity_gap_for_environment_selection() {
    let mut settings = settings();
    settings.config.providers[0].models.sonnet = "env-model".into();
    let selection = BTreeMap::from([
        ("MODEL_PROVIDER".to_owned(), "responses".to_owned()),
        ("MODEL_TYPE".to_owned(), "sonnet".to_owned()),
    ]);
    assert!(matches!(
        try_resolve(&settings, &selection),
        Err(ProviderError::MissingModelCapacity)
    ));
    assert_eq!(try_resolve_snapshot(&settings, &selection).unwrap(), None);
}

/// provider 级错误（缺 API key）即使同时缺容量也必须拒绝持久化，容量缺失不得掩盖它。
#[test]
fn test_snapshot_resolution_rejects_auxiliary_provider_error_hidden_by_missing_capacity() {
    let mut settings = settings();
    settings.config.providers.push(ProviderConfig {
        id: "other".into(),
        provider_type: "anthropic".into(),
        api_key: "other-key".into(),
        ..Default::default()
    });
    settings.config.profiles.opus.provider = "other".into();
    settings.config.profiles.opus.context_window = None;
    settings.config.profiles.haiku.context_window = None;
    settings.config.providers[0].api_key.clear();
    assert!(matches!(
        try_resolve_snapshot(&settings, &BTreeMap::new()),
        Err(ProviderError::MissingApiKey)
    ));
}

/// 生效档位的 provider 级错误仍拒绝持久化（不能因容量容错而放宽认证/端点校验）。
#[test]
fn test_snapshot_resolution_rejects_effective_provider_error() {
    let mut settings = settings();
    settings.config.profiles.haiku.context_window = None;
    settings.config.providers[0].api_key.clear();
    assert!(matches!(
        try_resolve_snapshot(&settings, &BTreeMap::new()),
        Err(ProviderError::MissingApiKey)
    ));
}

#[test]
fn test_snapshot_resolution_rejects_unbound_responses_provider_errors() {
    let mut settings = settings();
    let mut invalid_providers = Vec::new();

    let mut missing_key = settings.config.providers[0].clone();
    missing_key.api_key.clear();
    invalid_providers.push((missing_key, "missing key", ProviderError::MissingApiKey));

    let mut bad_endpoint = settings.config.providers[0].clone();
    bad_endpoint.base_url = "not-a-url".into();
    invalid_providers.push((
        bad_endpoint,
        "invalid endpoint",
        ProviderError::InvalidEndpoint,
    ));

    let mut bad_reference = settings.config.providers[0].clone();
    bad_reference.api_key.clear();
    bad_reference.auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: "relative/store".into(),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    invalid_providers.push((
        bad_reference.clone(),
        "invalid subscription reference",
        ProviderError::InvalidSubscriptionReference,
    ));

    bad_reference.auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: subscription_store(),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    bad_reference.base_url = "https://example.invalid/v1/responses".into();
    invalid_providers.push((
        bad_reference,
        "invalid subscription endpoint",
        ProviderError::InvalidSubscriptionEndpoint,
    ));

    for (mut provider, case, expected) in invalid_providers {
        provider.id = "unbound".into();
        provider.models = Default::default();
        settings.config.providers.push(provider);
        let result = try_resolve_snapshot(&settings, &BTreeMap::new());
        assert!(
            matches!(&result, Err(error) if std::mem::discriminant(error) == std::mem::discriminant(&expected)),
            "{case}: {result:?}"
        );
        settings.config.providers.pop();
    }
}

#[test]
fn test_snapshot_resolution_rejects_unbound_non_responses_provider_errors() {
    let mut settings = settings();
    settings.config.providers.push(ProviderConfig {
        id: "unbound".into(),
        provider_type: "unknown".into(),
        ..Default::default()
    });
    assert!(matches!(
        try_resolve_snapshot(&settings, &BTreeMap::new()),
        Err(ProviderError::UnknownType)
    ));

    let unbound = settings.config.providers.last_mut().unwrap();
    unbound.provider_type = "openai".into();
    unbound.auth = Some(ProviderAuthConfig::ApiKey);
    assert!(matches!(
        try_resolve_snapshot(&settings, &BTreeMap::new()),
        Err(ProviderError::UnexpectedAuth)
    ));
}

#[test]
fn test_snapshot_resolution_accepts_valid_unbound_responses_provider() {
    let mut settings = settings();
    let mut provider = settings.config.providers[0].clone();
    provider.id = "unbound".into();
    provider.models = Default::default();
    settings.config.providers.push(provider);
    assert!(matches!(
        try_resolve_snapshot(&settings, &BTreeMap::new()),
        Ok(Some(ResolvedProvider::Responses { .. }))
    ));
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
        store: subscription_store(),
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
