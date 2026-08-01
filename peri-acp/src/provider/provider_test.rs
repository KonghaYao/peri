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
    // PreparedModelRequest 是有意的安全观测投影：endpoint path 被脱敏为 /[REDACTED]，
    // host 保留。协议补全路径（/v1/chat/completions）只发生在私有请求构造期。
    assert_eq!(prepared.endpoint().host_str(), Some("api.example.com"));
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
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
    // 同 OpenAI：host 保留，path 在观测投影中脱敏。
    assert_eq!(prepared.endpoint().host_str(), Some("api.anthropic.com"));
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
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
fn into_model_anthropic_extended_thinking_applied() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 64000,
        context_1m: false,
    };
    let body = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    // budget_tokens = max_tokens（Profile 唯一事实源）
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 64000 })
    );
    assert_eq!(
        body["output_config"],
        serde_json::json!({ "effort": "high" })
    );
    assert_eq!(body["max_tokens"], serde_json::json!(64000));
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
    };
    let model = provider.into_model();
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    // 非法 base_url 回落到默认 endpoint（api.openai.com），host 保留，path 脱敏。
    assert_eq!(prepared.endpoint().host_str(), Some("api.openai.com"));
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
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
