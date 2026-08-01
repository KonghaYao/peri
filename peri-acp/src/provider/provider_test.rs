//! `LlmProvider::into_model()` / `context_window()` 的强类型映射测试。
//!
//! 冻结 Task 7 的协议边界：factory 产出 `Box<dyn peri_model::Model>`，
//! 而非旧 LLM facade trait。环境读取仍只发生在 ACP
//! （`from_env` / `from_config`），`peri-model` 不解析任何环境变量。

use super::*;
use crate::provider::ThinkingConfig;

fn openai_provider(model: &str) -> LlmProvider {
    LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: model.to_string(),
        thinking: None,
    }
}

fn anthropic_provider(model: &str) -> LlmProvider {
    LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: model.to_string(),
        base_url: None,
        thinking: None,
    }
}

fn think(enabled: bool, budget_tokens: u32, effort: &str, max_tokens: u32) -> ThinkingConfig {
    ThinkingConfig {
        enabled,
        budget_tokens,
        effort: effort.to_string(),
        max_tokens,
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
    // max_tokens 语义：into_model 统一从 thinking.max_tokens 读取（无 thinking 时 32000）。
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
        thinking: Some(think(true, 8000, "medium", 16384)),
    };
    let body = provider_with_think
        .into_model()
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("prepare_request 必须成功")
        .body()
        .as_value()
        .clone();
    assert_eq!(body["max_tokens"], serde_json::json!(16384));
    // thinking 配置透传：reasoning_effort + thinking.enabled
    assert_eq!(body["reasoning_effort"], serde_json::json!("medium"));
    assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }));
}

#[test]
fn into_model_anthropic_extended_thinking_applied() {
    let provider = LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        base_url: None,
        thinking: Some(think(true, 16000, "high", 64000)),
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
        serde_json::json!({ "type": "enabled", "budget_tokens": 16000 })
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
        thinking: None,
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
