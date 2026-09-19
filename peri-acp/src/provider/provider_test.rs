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
        api: ApiProtocol::ChatCompletions,
        effort: None,
        max_tokens: 32000,
        context_1m: false,
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
        api: ApiProtocol::ChatCompletions,
        effort: Some("medium".to_string()),
        max_tokens: 16384,
        context_1m: false,
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
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 4_095 })
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
        api: ApiProtocol::ChatCompletions,
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
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

// ─── API 协议工厂（Responses 真实工厂验证）──────────────────────────────────

/// 构造单 provider（openai 类型）的 PeriConfig：sonnet 档绑定 p1，max_tokens 4096。
fn peri_config_with_openai_provider(api: Option<ApiProtocol>, base_url: &str) -> PeriConfig {
    PeriConfig {
        config: AppConfig {
            active_alias: "sonnet".into(),
            profiles: Profiles {
                sonnet: ProfileConfig {
                    provider: "p1".into(),
                    effort: "high".into(),
                    max_tokens: 4096,
                    ..Default::default()
                },
                ..Default::default()
            },
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "openai".into(),
                api_key: "k".into(),
                base_url: base_url.into(),
                api,
                models: ProviderModels {
                    sonnet: "gpt-5.6".into(),
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
fn from_config_responses_protocol_builds_responses_factory() {
    let cfg = peri_config_with_openai_provider(
        Some(ApiProtocol::Responses),
        "https://api.example.com/v1",
    );
    let provider = LlmProvider::from_config(&cfg).expect("responses provider 应可构造");
    assert_eq!(provider.api_protocol(), Some(ApiProtocol::Responses));
    assert_eq!(provider.protocol_key(), "responses");

    let prepared = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::OpenAiResponses
    ));
    assert_eq!(prepared.model_id(), "gpt-5.6");
    assert_eq!(prepared.endpoint().host_str(), Some("api.example.com"));
    // 观测投影按既有规则脱敏 path
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
    let body = prepared.body().as_value();
    // Responses 无状态请求：store=false；上限走 max_output_tokens；effort 走 reasoning.effort
    assert_eq!(body["store"], serde_json::json!(false));
    // 无状态续轮依赖密文 reasoning：include 由 adapter 提供，工厂不得覆盖
    assert_eq!(
        body["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
    assert_eq!(body["max_output_tokens"], serde_json::json!(4096));
    assert_eq!(body["reasoning"], serde_json::json!({ "effort": "high" }));
    assert_eq!(body["stream"], serde_json::json!(true));
    // 不得混入 Chat Completions 字段（协议不可回退）
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("thinking").is_none());
}

#[test]
fn from_config_openai_without_api_defaults_to_chat_completions() {
    let cfg = peri_config_with_openai_provider(None, "https://api.example.com/v1");
    let provider = LlmProvider::from_config(&cfg).expect("缺省协议 provider 应可构造");
    assert_eq!(provider.api_protocol(), Some(ApiProtocol::ChatCompletions));
    assert_eq!(provider.protocol_key(), "chat_completions");
    let prepared = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::OpenAiCompatible
    ));
}

#[test]
fn from_config_responses_rejects_invalid_or_credentialed_base_url() {
    // Responses fail-closed：非法 endpoint 拒绝构造，不回落也不改发其他服务
    for bad in [
        "not a url",
        "ftp://api.example.com/v1",
        "https://user:pass@api.example.com/v1",
    ] {
        let cfg = peri_config_with_openai_provider(Some(ApiProtocol::Responses), bad);
        assert!(
            LlmProvider::from_config(&cfg).is_none(),
            "非法 responses base_url 必须拒绝构造"
        );
    }
    // 旧协议保持 fail-soft：chat_completions 非法 URL 不因本次改动收紧
    let cfg = peri_config_with_openai_provider(Some(ApiProtocol::ChatCompletions), "not a url");
    assert!(LlmProvider::from_config(&cfg).is_some());
}

#[test]
fn from_config_responses_empty_base_url_uses_default_endpoint() {
    let cfg = peri_config_with_openai_provider(Some(ApiProtocol::Responses), "");
    let prepared = LlmProvider::from_config(&cfg)
        .expect("空 base_url 走静态默认 endpoint")
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert_eq!(prepared.endpoint().host_str(), Some("api.openai.com"));
    assert_eq!(prepared.endpoint().path(), "/[REDACTED]");
}

#[test]
fn from_config_rejects_anthropic_with_explicit_api() {
    let mut cfg = peri_config_with_openai_provider(Some(ApiProtocol::ChatCompletions), "");
    cfg.config.providers[0].provider_type = "anthropic".into();
    assert!(
        LlmProvider::from_config(&cfg).is_none(),
        "anthropic + 显式 api 是非法组合，必须拒绝"
    );

    // 未声明 api 的 anthropic 保持既有语义
    cfg.config.providers[0].api = None;
    let provider = LlmProvider::from_config(&cfg).expect("anthropic 缺省 api 仍合法");
    assert_eq!(provider.api_protocol(), None);
    assert_eq!(provider.protocol_key(), "anthropic");
    assert_eq!(provider.display_name(), "Anthropic");
}

#[test]
fn protocol_key_distinguishes_openai_protocols() {
    // 同 model / base_url / 凭据下，协议参与 provider 身份
    let mut chat = openai_provider("gpt-5.6");
    let mut responses = openai_provider("gpt-5.6");
    if let LlmProvider::OpenAi { api, .. } = &mut chat {
        *api = ApiProtocol::ChatCompletions;
    }
    if let LlmProvider::OpenAi { api, .. } = &mut responses {
        *api = ApiProtocol::Responses;
    }
    assert_ne!(chat.protocol_key(), responses.protocol_key());
    assert_eq!(chat.protocol_key(), "chat_completions");
    assert_eq!(responses.protocol_key(), "responses");
}

// ─── env 入口回归（R-1：旧 env 路径保持缺省 Chat）─────────────────────────────

/// `from_env` 读取进程环境，测试必须串行；进入时清空相关键并在 Drop 恢复原值，
/// 避免污染同二进制内的其他测试（并发测试同样可能间接读这些键）。
struct ProviderEnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl ProviderEnvGuard {
    const KEYS: [&'static str; 8] = [
        "MODEL_PROVIDER",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
        "OPENAI_API_BASE",
        "OPENAI_BASE_URL",
        "OPENAI_MODEL",
    ];

    fn apply(overrides: &[(&'static str, &str)]) -> Self {
        let saved = Self::KEYS
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in Self::KEYS {
            std::env::remove_var(key);
        }
        for (key, value) in overrides {
            std::env::set_var(key, value);
        }
        Self { saved }
    }
}

impl Drop for ProviderEnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// env 入口没有协议声明入口：`MODEL_PROVIDER=openai` 始终构造 Chat Completions
/// provider（协议选择只经配置字段与 UI，旧 env 行为不变）。
#[test]
#[serial_test::serial]
fn from_env_openai_stays_on_chat_completions() {
    let _env = ProviderEnvGuard::apply(&[
        ("MODEL_PROVIDER", "openai"),
        ("OPENAI_API_KEY", "sk-env-test-key"),
        ("OPENAI_BASE_URL", "https://env.example.com/v1"),
        ("OPENAI_MODEL", "gpt-env-model"),
    ]);
    let provider = LlmProvider::from_env().expect("env 路径必须可构造 provider");
    assert_eq!(provider.api_protocol(), Some(ApiProtocol::ChatCompletions));
    assert_eq!(provider.protocol_key(), "chat_completions");
    let prepared = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::OpenAiCompatible
    ));
    assert_eq!(prepared.model_id(), "gpt-env-model");
}

/// env 入口的 Anthropic 分支不受 api 字段影响：无协议维度，走原生 Messages API。
#[test]
#[serial_test::serial]
fn from_env_anthropic_has_no_api_protocol() {
    let _env = ProviderEnvGuard::apply(&[
        ("MODEL_PROVIDER", "anthropic"),
        ("ANTHROPIC_API_KEY", "sk-ant-env-test-key"),
        ("ANTHROPIC_MODEL", "claude-env-model"),
    ]);
    let provider = LlmProvider::from_env().expect("env 路径必须可构造 provider");
    assert_eq!(provider.api_protocol(), None);
    assert_eq!(provider.protocol_key(), "anthropic");
    let prepared = provider
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功");
    assert!(matches!(
        prepared.protocol(),
        peri_model::ProviderProtocol::Anthropic
    ));
    assert_eq!(prepared.model_id(), "claude-env-model");
}
