use std::collections::BTreeMap;

use serde_json::{json, Value};
use url::Url;

use super::{ModelRuntimeConfig, PreparedModelRequest};
use crate::{ProtocolErrorKind, ProviderProtocol};

fn observed(body: Value, runtime: &ModelRuntimeConfig) -> PreparedModelRequest {
    PreparedModelRequest::observe_with_runtime(
        ProviderProtocol::OpenAiCompatible,
        "gpt-test",
        Url::parse(
            "https://user:password@api.example.test/private?api_key=sk-live-secret#fragment",
        )
        .unwrap(),
        body,
        BTreeMap::from([("Authorization".into(), json!("Bearer secret"))]),
        runtime,
    )
    .unwrap()
}

#[test]
fn observation_preserves_all_content_and_endpoint_components() {
    let body = json!({
        "headers": {"Authorization": "Bearer secret"},
        "api_key": "sk-live-secret",
        "credentials": {"password": "secret"},
        "access_token": "secret-token",
        "Cookie": "session=secret",
        "һеаders": {"Authorization": "Unicode secret"},
        "工具/参数~": "诊断",
        "image_url": "data:image/png;base64,secret",
        "budget_tokens": 16000,
    });
    let request = observed(body.clone(), &ModelRuntimeConfig::default());
    assert_eq!(request.body().as_value(), &body);
    assert_eq!(request.metadata()["Authorization"], json!("Bearer secret"));
    assert_eq!(
        request.endpoint().as_str(),
        "https://user:password@api.example.test/private?api_key=sk-live-secret#fragment"
    );
    assert!(request.truncated_paths().is_empty());
    let debug = format!("{request:?}");
    for content in [
        "Bearer secret",
        "sk-live-secret",
        "Unicode secret",
        "data:image/png;base64,secret",
        "工具/参数~",
    ] {
        assert!(debug.contains(content), "missing {content}: {debug}");
    }
    let serialized = serde_json::to_value(&request).unwrap();
    assert_eq!(serialized["body"], body);
}

#[test]
fn observation_preserves_length_limits_and_unicode_paths() {
    let request = observed(
        json!({"工具/参数~": "界".repeat(4097)}),
        &ModelRuntimeConfig::default(),
    );
    assert_eq!(
        request.body().as_value()["工具/参数~"],
        json!("[TRUNCATED]")
    );
    assert_eq!(request.truncated_paths(), &["/工具~1参数~0"]);
    let request = observed(
        json!({"prompt": "界".repeat(4096)}),
        &ModelRuntimeConfig::default(),
    );
    assert_eq!(
        request.body().as_value()["prompt"],
        json!("界".repeat(4096))
    );
}

#[test]
fn explicit_full_observation_preserves_large_content() {
    let body = json!({"prompt": "x".repeat(10_000), "api_key": "secret"});
    let request = observed(body.clone(), &ModelRuntimeConfig::with_full_observation());
    assert_eq!(request.body().as_value(), &body);
    assert!(request.truncated_paths().is_empty());
}

#[test]
fn observation_rejects_non_http_endpoints() {
    for endpoint in [
        "mailto:secret@example.test",
        "data:text/plain,secret",
        "file:///secret",
    ] {
        let error = PreparedModelRequest::observe(
            ProviderProtocol::OpenAiCompatible,
            "test",
            Url::parse(endpoint).unwrap(),
            json!({}),
            BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.protocol_error().unwrap().kind(),
            ProtocolErrorKind::InvalidEndpoint
        );
    }
}
