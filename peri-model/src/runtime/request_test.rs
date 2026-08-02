use std::collections::BTreeMap;

use serde_json::json;
use url::Url;

use super::{ModelRuntimeConfig, PreparedModelRequest};
use crate::ProviderProtocol;

fn prepared_request(body: serde_json::Value) -> PreparedModelRequest {
    PreparedModelRequest::observe(
        ProviderProtocol::OpenAiCompatible,
        "gpt-test",
        Url::parse("https://api.example.test/v1/chat/completions?api_key=sk-live-secret").unwrap(),
        body,
        BTreeMap::from([
            ("session_id".into(), json!("session_123")),
            ("authorization".into(), json!("Bearer sk-live-secret")),
        ]),
    )
    .unwrap()
}

#[test]
fn test_endpoint_projection_removes_input_path_and_credentials() {
    let observed = PreparedModelRequest::observe(
        ProviderProtocol::OpenAiCompatible,
        "gpt-test",
        Url::parse("https://user:endpoint-password-secret@api.example.test/v1/sk-live-secret?api_key=sk-live-secret#fragment-secret").unwrap(),
        json!({}),
        BTreeMap::new(),
    )
    .unwrap();
    let serialized = serde_json::to_string(&observed).unwrap();
    let debug = format!("{observed:?}");

    assert_eq!(
        observed.endpoint().as_str(),
        "https://api.example.test/[REDACTED]"
    );
    assert!(!serialized.contains("sk-live-secret"));
    assert!(!serialized.contains("endpoint-password-secret"));
    assert!(!serialized.contains("fragment-secret"));
    assert!(!debug.contains("sk-live-secret"));
    assert!(!debug.contains("endpoint-password-secret"));
}

#[test]
fn test_observed_request_has_no_credentials_or_headers() {
    let observed = prepared_request(json!({
        "api_key": "sk-live-secret",
        "Authorization": "Bearer sk-live-secret",
        "Cookie": "session=secret",
        "messages": [{"content": "safe"}],
    }));
    let json = serde_json::to_string(&observed).unwrap();

    assert!(!json.contains("sk-live-secret"));
    assert!(!json.contains("Bearer"));
    assert!(!json.contains("\"Cookie\":"));
    assert_eq!(
        observed.endpoint().as_str(),
        "https://api.example.test/[REDACTED]"
    );
}

#[test]
fn test_oversized_tool_output_is_replaced_and_its_path_is_recorded() {
    let observed = prepared_request(json!({
        "messages": [{"content": "x".repeat(100_000)}],
    }));

    assert_eq!(
        observed.body().as_value().pointer("/messages/0/content"),
        Some(&json!("[TRUNCATED]"))
    );
    assert!(observed
        .truncated_paths()
        .contains(&"/messages/0/content".into()));
}

#[test]
fn test_sensitive_values_and_data_uris_are_redacted_with_paths() {
    let observed = prepared_request(json!({
        "nested": {
            "client_secret": "secret-value",
            "image_url": "data:image/png;base64,secret-image-data",
        },
    }));

    assert_eq!(
        observed.body().as_value().pointer("/nested/client_secret"),
        None
    );
    assert_eq!(
        observed.body().as_value().pointer("/nested/image_url"),
        Some(&json!("[REDACTED]"))
    );
    assert_eq!(
        observed.redacted_paths(),
        [
            "/nested/client_secret",
            "/nested/image_url",
            "/metadata/authorization"
        ]
    );
}

#[test]
fn test_header_and_credential_containers_are_removed_without_recursing() {
    let observed = PreparedModelRequest::observe(
        ProviderProtocol::OpenAiCompatible,
        "gpt-test",
        Url::parse("https://api.example.test/v1/chat/completions").unwrap(),
        json!({
            "headers": {"safe-looking": "sk-live-secret"},
            "proxy_authorization": "Bearer sk-live-secret",
            "Set-Cookie": "session=secret",
            "credentials": {"nested": {"prompt": "secret prompt"}},
            "safe": "retained",
        }),
        BTreeMap::from([
            ("Headers".into(), json!({"safe-looking": "sk-live-secret"})),
            ("proxy-authorization".into(), json!("Bearer sk-live-secret")),
            ("set_cookie".into(), json!("session=secret")),
            ("credential".into(), json!({"nested": "secret prompt"})),
            ("safe".into(), json!("retained")),
        ]),
    )
    .unwrap();
    let serialized = serde_json::to_string(&observed).unwrap();

    assert!(!serialized.contains("sk-live-secret"));
    assert!(!serialized.contains("secret prompt"));
    assert_eq!(observed.body().as_value().pointer("/headers"), None);
    assert_eq!(observed.body().as_value().pointer("/credentials"), None);
    assert_eq!(
        observed.body().as_value().pointer("/safe"),
        Some(&json!("retained"))
    );
    assert_eq!(observed.metadata().get("safe"), Some(&json!("retained")));
    let mut redacted_paths = observed.redacted_paths().to_vec();
    redacted_paths.sort();
    assert_eq!(
        redacted_paths,
        [
            "/Set-Cookie",
            "/credentials",
            "/headers",
            "/metadata/Headers",
            "/metadata/credential",
            "/metadata/proxy-authorization",
            "/metadata/set_cookie",
            "/proxy_authorization",
        ]
    );
}

#[test]
fn test_non_ascii_keys_are_removed_without_leaking_their_values_or_names() {
    let observed = PreparedModelRequest::observe(
        ProviderProtocol::OpenAiCompatible,
        "gpt-test",
        Url::parse("https://api.example.test/v1/chat/completions").unwrap(),
        json!({
            "һеаders": {"Authorization": "Bearer sk-cyrillic-header-secret"},
            "арiКеу": {"nested": "sk-cyrillic-api-key-secret"},
            "сredentials": {"nested": "sk-cyrillic-credentials-secret"},
            "用户内容": "legitimate user content",
            "safe": "retained",
        }),
        BTreeMap::from([
            (
                "һеаders".into(),
                json!({"Authorization": "Bearer sk-cyrillic-metadata-secret"}),
            ),
            ("用户内容".into(), json!("legitimate metadata content")),
            ("safe".into(), json!("retained")),
        ]),
    )
    .unwrap();
    let serialized = serde_json::to_string(&observed).unwrap();

    for secret in [
        "sk-cyrillic-header-secret",
        "sk-cyrillic-api-key-secret",
        "sk-cyrillic-credentials-secret",
        "sk-cyrillic-metadata-secret",
        "legitimate user content",
        "legitimate metadata content",
        "һеаders",
        "арiКеу",
        "сredentials",
        "用户内容",
    ] {
        assert!(!serialized.contains(secret));
    }
    assert_eq!(
        observed.body().as_value().pointer("/safe"),
        Some(&json!("retained"))
    );
    assert_eq!(observed.metadata().get("safe"), Some(&json!("retained")));
    assert_eq!(
        observed.redacted_paths(),
        [
            "/[NON_ASCII_KEY]",
            "/[NON_ASCII_KEY]",
            "/[NON_ASCII_KEY]",
            "/[NON_ASCII_KEY]",
            "/metadata/[NON_ASCII_KEY]",
            "/metadata/[NON_ASCII_KEY]",
        ]
    );
}

#[test]
fn test_prepared_request_getters_expose_only_safe_projection() {
    let observed = prepared_request(json!({"safe": "value", "headers": {"x": "secret"}}));

    assert_eq!(observed.protocol(), &ProviderProtocol::OpenAiCompatible);
    assert_eq!(observed.model_id(), "gpt-test");
    assert_eq!(
        observed.endpoint().as_str(),
        "https://api.example.test/[REDACTED]"
    );
    assert_eq!(
        observed.body().as_value().pointer("/safe"),
        Some(&json!("value"))
    );
    assert!(!observed.metadata().contains_key("authorization"));
    assert_eq!(
        observed.redacted_paths(),
        ["/headers", "/metadata/authorization"]
    );
    assert_eq!(
        serde_json::to_value(&observed)
            .unwrap()
            .pointer("/body/safe"),
        Some(&json!("value"))
    );
}

#[test]
fn test_runtime_opt_in_is_internal_and_never_restores_sensitive_values() {
    let config = ModelRuntimeConfig::with_full_observation();
    let observed = PreparedModelRequest::observe_with_runtime(
        ProviderProtocol::Anthropic,
        "claude-test",
        Url::parse("https://api.example.test/v1/messages").unwrap(),
        json!({
            "prompt": "x".repeat(10_000),
            "api_key": "sk-live-secret",
            "һеаders": {"Authorization": "sk-cyrillic-secret"},
        }),
        BTreeMap::new(),
        &config,
    )
    .unwrap();

    assert_eq!(
        observed.body().as_value().pointer("/prompt"),
        Some(&json!("x".repeat(10_000)))
    );
    assert_eq!(observed.body().as_value().pointer("/api_key"), None);
    assert_eq!(observed.body().as_value().pointer("/[NON_ASCII_KEY]"), None);
    assert!(observed.truncated_paths().is_empty());
    assert!(observed
        .redacted_paths()
        .contains(&"/[NON_ASCII_KEY]".into()));
}

#[test]
fn test_public_default_observation_always_remains_restricted() {
    let observed = PreparedModelRequest::observe(
        ProviderProtocol::Anthropic,
        "claude-test",
        Url::parse("https://api.example.test/v1/messages").unwrap(),
        json!({"prompt": "x".repeat(10_000), "api_key": "sk-live-secret"}),
        BTreeMap::new(),
    )
    .unwrap();

    assert_eq!(
        observed.body().as_value().pointer("/prompt"),
        Some(&json!("[TRUNCATED]"))
    );
    assert_eq!(observed.body().as_value().pointer("/api_key"), None);
}

#[test]
fn test_prepared_request_debug_redacts_body_and_metadata() {
    let observed = prepared_request(json!({"prompt": "very long user prompt"}));
    let debug = format!("{observed:?}");

    assert!(!debug.contains("very long user prompt"));
    assert!(!debug.contains("sk-live-secret"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn test_endpoint_observation_rejects_non_http_urls_without_leaking_input() {
    for endpoint in [
        "mailto:secret@example.test",
        "data:text/plain,sk-live-secret",
        "file:///relative-path-with-sk-live-secret",
    ] {
        let result = std::panic::catch_unwind(|| {
            PreparedModelRequest::observe(
                ProviderProtocol::OpenAiCompatible,
                "gpt-test",
                Url::parse(endpoint).unwrap(),
                json!({}),
                BTreeMap::new(),
            )
        });

        let error = result
            .expect("endpoint observation must not panic")
            .expect_err("endpoint observation must fail closed");
        let rendered = format!("{error:?} {error}");
        assert_eq!(
            error.protocol_error().map(|error| error.kind()),
            Some(crate::ProtocolErrorKind::InvalidEndpoint)
        );
        assert!(!rendered.contains(endpoint));
        assert!(!rendered.contains("sk-live-secret"));
    }
}

#[test]
fn test_endpoint_observation_redacts_userinfo_for_http_urls() {
    let observed = PreparedModelRequest::observe(
        ProviderProtocol::OpenAiCompatible,
        "gpt-test",
        Url::parse("https://user:password-secret@api.example.test/v1/secret?api_key=sk-live-secret#fragment-secret").unwrap(),
        json!({}),
        BTreeMap::new(),
    )
    .unwrap();

    assert_eq!(
        observed.endpoint().as_str(),
        "https://api.example.test/[REDACTED]"
    );
}

#[test]
fn test_token_suffix_usage_fields_are_kept_but_singular_token_credentials_removed() {
    // 回归：`budget_tokens` 等复数计数字段不得因 contains("token") 被误删
    // （Anthropic extended thinking 的 budget 会因此从观测投影中消失）。
    let observed = prepared_request(json!({
        "thinking": {"type": "enabled", "budget_tokens": 16000},
        "max_tokens": 32000,
        "output_config": {"effort": "high"},
        "usage": {
            "input_tokens": 1,
            "output_tokens": 2,
            "cache_read_input_tokens": 3,
            "cache_creation_input_tokens": 4,
            "total_tokens": 5,
        },
        "access_token": "sk-live-secret",
        "api_token": "sk-live-secret",
    }));
    let body = observed.body().as_value();

    assert_eq!(body.pointer("/thinking/budget_tokens"), Some(&json!(16000)));
    assert_eq!(body.pointer("/max_tokens"), Some(&json!(32000)));
    assert_eq!(body.pointer("/output_config/effort"), Some(&json!("high")));
    assert_eq!(body.pointer("/usage/input_tokens"), Some(&json!(1)));
    assert_eq!(body.pointer("/usage/output_tokens"), Some(&json!(2)));
    assert_eq!(
        body.pointer("/usage/cache_read_input_tokens"),
        Some(&json!(3))
    );
    assert_eq!(
        body.pointer("/usage/cache_creation_input_tokens"),
        Some(&json!(4))
    );
    assert_eq!(body.pointer("/usage/total_tokens"), Some(&json!(5)));
    assert_eq!(body.pointer("/access_token"), None);
    assert_eq!(body.pointer("/api_token"), None);
    assert!(!serde_json::to_string(&observed)
        .unwrap()
        .contains("sk-live-secret"));
}
