use super::AnthropicConfig;
use url::Url;

#[test]
fn config_debug_preserves_credential_and_endpoint() {
    let config = AnthropicConfig::new(
        Url::parse("https://user:password@proxy.example.test/private?api_key=secret#fragment")
            .expect("valid URL"),
        "test-credential",
        "claude-test",
    );
    let rendered = format!("{config:?}");
    for sensitive in [
        "user",
        "password",
        "private",
        "api_key=secret",
        "fragment",
        "test-credential",
    ] {
        assert!(
            rendered.contains(sensitive),
            "Debug output exposed {sensitive:?}: {rendered}"
        );
    }
    assert!(!rendered.contains("[REDACTED]"));
}

#[test]
fn messages_endpoint_preserves_base_path_and_userinfo() {
    let endpoint = super::request::messages_endpoint(
        &Url::parse("https://proxy.example.test/custom/").expect("valid URL"),
    )
    .expect("messages endpoint");
    assert_eq!(
        endpoint.as_str(),
        "https://proxy.example.test/custom/v1/messages"
    );
    let endpoint = super::request::messages_endpoint(
        &Url::parse("https://user:password@proxy.example.test/custom").expect("valid URL"),
    )
    .expect("userinfo is preserved");
    assert_eq!(
        endpoint.as_str(),
        "https://user:password@proxy.example.test/custom/v1/messages"
    );
}
