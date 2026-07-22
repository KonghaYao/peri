//! Tests for setup_wizard
use super::*;
use serial_test::serial;

#[test]
#[serial]
fn test_needs_setup_empty_providers_no_env() {
    let config = crate::config::AppConfig::default();
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
    assert!(
        needs_setup(&config),
        "无 providers 且无有效 env 时应需要 setup"
    );
}

#[test]
#[serial]
fn test_needs_setup_api_key_from_config() {
    let mut config = crate::config::AppConfig::default();
    config.providers.push(crate::config::ProviderConfig {
        id: "test".into(),
        provider_type: "openai".into(),
        api_key: "sk-fake-test-key".into(),
        ..Default::default()
    });
    assert!(!needs_setup(&config));
}

#[test]
fn test_provider_type_cycle() {
    let mut pt = ProviderType::Anthropic;
    pt.cycle();
    assert_eq!(pt, ProviderType::OpenAiCompatible);
    pt.cycle();
    assert_eq!(pt, ProviderType::Anthropic);
}

#[test]
fn test_migrated_provider_is_complete() {
    let mp = MigratedProvider::new(ProviderType::Anthropic);
    // 新创建的 provider api_key 为空，不完整
    assert!(!mp.is_complete());

    let mut mp2 = MigratedProvider::new(ProviderType::Anthropic);
    mp2.api_key = "sk-test".to_string();
    assert!(mp2.is_complete());
}

#[test]
fn test_mask_api_key() {
    assert_eq!(mask_api_key("sk-short"), "••••••••");
    assert_eq!(
        mask_api_key("sk-ant-api03-very-long-key-here"),
        "sk-a••••here"
    );
}

#[test]
fn test_parse_url_parts_standard() {
    let (host, port, path) = parse_url_parts("https://api.anthropic.com").expect("parse failed");
    assert_eq!(host, "api.anthropic.com");
    assert_eq!(port, 443);
    assert_eq!(path, "/");
}

#[test]
fn test_parse_url_parts_with_path() {
    let (host, port, path) =
        parse_url_parts("http://localhost:8080/v1/chat").expect("parse failed");
    assert_eq!(host, "localhost");
    assert_eq!(port, 8080);
    assert_eq!(path, "/v1/chat");
}
