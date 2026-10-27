//! `LlmProvider::into_model()` / `context_window()` 的强类型映射测试。
//!
//! 冻结 Task 7 的协议边界：factory 产出 `Box<dyn peri_model::Model>`，
//! 而非旧 LLM facade trait。环境读取仍只发生在 ACP
//! （`from_env` / `from_config`），`peri-model` 不解析任何环境变量。

use super::*;

fn openai_provider(model: &str) -> LlmProvider {
    LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: model.to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    }
}

fn anthropic_provider(model: &str) -> LlmProvider {
    LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: model.to_string(),
        base_url: None,
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    }
}

#[test]
fn into_model_openai_produces_openai_compatible_protocol() {
    let model = openai_provider("gpt-4o").into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::OpenAiCompatible
    ));
    assert_eq!(prepared.model_id(), "gpt-4o");
    assert_eq!(prepared.endpoint().host_str(), Some("api.example.com"));
    assert_eq!(prepared.endpoint().path(), "/v1/chat/completions");
}

#[test]
fn into_model_anthropic_produces_anthropic_protocol() {
    let model = anthropic_provider("claude-sonnet-4-6").into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::Anthropic
    ));
    assert_eq!(prepared.model_id(), "claude-sonnet-4-6");
    assert_eq!(prepared.endpoint().host_str(), Some("api.anthropic.com"));
    assert_eq!(prepared.endpoint().path(), "/v1/messages");
}

#[test]
fn into_model_thinking_config_applies_max_tokens() {
    // max_tokens 语义：into_model 从 provider 的 max_tokens 读取（默认 32000）。
    let model = openai_provider("gpt-4o")
        .with_model_name("gpt-4o".to_string())
        .into_model();
    let body = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(body["max_tokens"], serde_json::json!(32000));

    let provider_with_think = LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: "gpt-4o".to_string(),
        effort: Some("medium".to_string()),
        max_tokens: 16384,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    };
    let body = provider_with_think
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(body["max_tokens"], serde_json::json!(16384));
    // effort 配置透传：reasoning_effort + thinking.enabled
    assert_eq!(body["reasoning_effort"], serde_json::json!("medium"));
    assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }));
}

#[test]
fn with_max_tokens_overrides_output_limit_without_changing_model() {
    let model = openai_provider("gpt-4o").with_max_tokens(4096).into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");

    assert_eq!(prepared.model_id(), "gpt-4o");
    assert_eq!(
        prepared.body().as_value()["max_tokens"],
        serde_json::json!(4096)
    );
}

#[test]
fn into_model_anthropic_extended_thinking_applied() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 64000,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    };
    let body = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 10_000 })
    );
    assert_eq!(
        body["output_config"],
        serde_json::json!({ "effort": "high" })
    );
    assert_eq!(body["max_tokens"], serde_json::json!(64000));
}

#[test]
fn workflow_output_limit_disables_invalid_anthropic_thinking_budget() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 64_000,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    }
    .with_max_tokens(1_024);

    let body = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();

    assert_eq!(body["max_tokens"], serde_json::json!(1_024));
    assert!(body.get("thinking").is_none());
    assert!(body.get("output_config").is_none());
}

#[test]
fn workflow_output_limit_clamps_anthropic_thinking_below_total_limit() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 64_000,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    }
    .with_max_tokens(4_096);

    let body = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();

    assert_eq!(body["max_tokens"], serde_json::json!(4_096));
    // thinking 与正文共享输出上限：为正文预留 1024 后，思考预算为 3072。
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 3_072 })
    );
}

#[test]
fn into_model_invalid_base_url_falls_back_without_panic() {
    // fail-soft：非法 base_url 不 panic，回落到默认 endpoint；
    // 真正无效的 endpoint 由协议层在 prepare/stream 时 fail closed。
    let provider = LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "not a url".to_string(),
        model: "gpt-4o".to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    };
    let model = provider.into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert_eq!(prepared.endpoint().host_str(), Some("api.openai.com"));
    assert_eq!(prepared.endpoint().path(), "/v1/chat/completions");
}

#[test]
fn context_window_is_200k_for_both_providers() {
    assert_eq!(openai_provider("gpt-4o").context_window(), 200_000);
    assert_eq!(
        anthropic_provider("claude-sonnet-4-6").context_window(),
        200_000
    );
}

#[test]
fn legacy_config_capacity_reaches_provider_and_real_model_metadata() {
    for provider_type in ["anthropic", "openai"] {
        let mut cfg = PeriConfig::default();
        cfg.config.active_alias = "opus".into();
        cfg.config.providers.push(ProviderConfig {
            id: "configured".into(),
            provider_type: provider_type.into(),
            api_key: "synthetic-key".into(),
            models: ProviderModels {
                opus: "synthetic-model".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        cfg.config.profiles.opus.provider = "configured".into();
        for (window, context_1m, expected) in [
            (Some(128_000), false, 128_000),
            (None, false, 200_000),
            (Some(0), false, 200_000),
            (Some(0), true, 1_000_000),
            (Some(128_000), true, 1_000_000),
            (None, true, 1_000_000),
        ] {
            cfg.config.profiles.opus.context_window = window;
            cfg.config.profiles.opus.context_1m = context_1m;
            let resolved = peri_config::provider::try_resolve_for_alias(&cfg, "opus")
                .unwrap()
                .unwrap();
            assert_eq!(resolved.effective_context_window(), expected);
            let provider = LlmProvider::from_resolved(resolved);
            assert_eq!(provider.context_window(), expected);
            assert_eq!(provider.effective_context_window(), expected);
            assert_eq!(provider.into_model().context_window(), Some(expected));
        }
    }
}

#[test]
fn from_config_reads_active_profile() {
    let cfg = PeriConfig {
        config: AppConfig {
            active_alias: "opus".into(),
            profiles: Profiles {
                opus: ProfileConfig {
                    provider: "p1".into(),
                    effort: "max".into(),
                    max_tokens: 64000,
                    context_1m: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "openai".into(),
                api_key: "k".into(),
                models: ProviderModels {
                    opus: "gpt-x".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let p = LlmProvider::from_config(&cfg).unwrap();
    assert_eq!(p.model_name(), "gpt-x");
    assert!(p.context_1m());
    assert_eq!(p.effort_key(), ":effort=max");
    let body = p
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(body["max_tokens"], serde_json::json!(64000));
    assert_eq!(body["reasoning_effort"], serde_json::json!("max"));
}

#[test]
fn from_config_for_alias_fable_falls_back_to_opus_model() {
    let cfg = PeriConfig {
        config: AppConfig {
            active_alias: "fable".into(),
            profiles: Profiles {
                fable: ProfileConfig {
                    provider: "p1".into(),
                    effort: "xhigh".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                api_key: "k".into(),
                models: ProviderModels {
                    opus: "claude-opus-4-6".into(),
                    fable: String::new(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let p = LlmProvider::from_config(&cfg).unwrap();
    // fable 档位 model 空 → 回退 opus
    assert_eq!(p.model_name(), "claude-opus-4-6");
    let _ = p;
}

#[test]
fn from_resolved_preserves_runtime_fields_for_both_protocols() {
    let resolved = [
        ResolvedProvider::Anthropic {
            api_key: "key".into(),
            model: "model".into(),
            base_url: Some("https://configured.example".into()),
            effort: Some("high".into()),
            max_tokens: 64000,
            context_1m: true,
            context_window: 1_000_000,
        },
        ResolvedProvider::OpenAi {
            api_key: "key".into(),
            model: "model".into(),
            base_url: "https://configured.example".into(),
            effort: Some("high".into()),
            max_tokens: 64000,
            context_1m: true,
            context_window: 1_000_000,
        },
    ];
    for provider in resolved {
        let runtime = LlmProvider::from_resolved(provider);
        assert_eq!(runtime.model_name(), "model");
        assert!(runtime.context_1m());
        assert_eq!(runtime.effort_key(), ":effort=high");
        match runtime {
            LlmProvider::Responses { .. } => panic!("legacy fixtures must remain legacy protocols"),
            LlmProvider::Anthropic {
                api_key,
                base_url,
                max_tokens,
                retry_observer,
                ..
            } => {
                assert_eq!(api_key, "key");
                assert_eq!(base_url.as_deref(), Some("https://configured.example"));
                assert_eq!(max_tokens, 64000);
                assert!(retry_observer.is_none());
            }
            LlmProvider::OpenAi {
                api_key,
                base_url,
                max_tokens,
                retry_observer,
                ..
            } => {
                assert_eq!(api_key, "key");
                assert_eq!(base_url, "https://configured.example");
                assert_eq!(max_tokens, 64000);
                assert!(retry_observer.is_none());
            }
        }
    }
}

#[test]
fn from_source_uses_resolved_snapshot_after_disk_changes() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"config":{"active_alias":"opus","providers":[{"id":"configured","type":"openai","apiKey":"key","models":{"opus":"frozen-model"}}]}}"#,
    )
    .unwrap();
    let source = ConfigSource::load_at(temporary.path(), path.clone()).unwrap();
    std::fs::write(&path, "{broken-later}").unwrap();

    let provider = LlmProvider::from_source(&source).unwrap();
    assert_eq!(provider.model_name(), "frozen-model");
    assert!(matches!(
        provider,
        LlmProvider::OpenAi {
            retry_observer: None,
            ..
        }
    ));
}
