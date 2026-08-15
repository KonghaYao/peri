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

#[test]
fn test_peri_free_provider_fields() {
    let mp = peri_free_provider();
    assert_eq!(mp.provider_id, "peri");
    assert_eq!(mp.base_url, PERI_FREE_BASE_URL);
    assert_eq!(mp.api_key, "public");
    assert_eq!(mp.provider_type, ProviderType::Anthropic);
    assert_eq!(mp.aliases, PERI_FREE_MODEL_IDS.map(String::from));
    assert!(mp.selected, "免费服务应默认选中");
    assert!(mp.is_complete(), "免费服务配置应视为完整");
}

#[test]
fn test_build_wizard_config_peri_free_profiles() {
    let state = SetupWizardState {
        step: SetupStep::Form,
        source: SetupSource::PeriFreeService,
        providers: vec![peri_free_provider()],
        language: "zh-CN".to_string(),
        ..Default::default()
    };
    let cfg = build_wizard_config(&state);
    assert_eq!(
        cfg.config.active_alias, "sonnet",
        "免费服务默认档位为 sonnet"
    );
    assert_eq!(cfg.config.providers.len(), 1);
    let p = &cfg.config.providers[0];
    assert_eq!(p.id, "peri");
    assert_eq!(p.base_url, PERI_FREE_BASE_URL);
    assert_eq!(p.api_key, "public");
    assert_eq!(
        [
            p.models.fable.as_str(),
            p.models.opus.as_str(),
            p.models.sonnet.as_str(),
            p.models.haiku.as_str()
        ],
        PERI_FREE_MODEL_IDS
    );
    assert_eq!(cfg.config.profiles.fable.effort, "max");
    assert_eq!(cfg.config.profiles.opus.effort, "medium");
    assert_eq!(cfg.config.profiles.sonnet.effort, "max");
    assert_eq!(cfg.config.profiles.haiku.effort, "low");
    for alias in ["fable", "opus", "sonnet", "haiku"] {
        assert_eq!(
            cfg.config.profiles.get(alias).unwrap().provider,
            "peri",
            "{alias} 档位应绑定 peri provider"
        );
    }
    assert_eq!(cfg.config.language.as_deref(), Some("zh-CN"));
}

#[test]
fn test_build_wizard_config_custom_api_keeps_opus_only() {
    let mut mp = MigratedProvider::new(ProviderType::Anthropic);
    mp.api_key = "sk-test".to_string();
    let state = SetupWizardState {
        step: SetupStep::Form,
        source: SetupSource::CustomApi,
        providers: vec![mp],
        ..Default::default()
    };
    let cfg = build_wizard_config(&state);
    assert_eq!(cfg.config.active_alias, "opus", "手动配置默认档位仍为 opus");
    assert_eq!(cfg.config.profiles.opus.provider, "anthropic");
    // 非 Peri 免费服务来源：其余档位保持默认
    assert!(cfg.config.profiles.fable.is_default());
    assert!(cfg.config.profiles.sonnet.is_default());
    assert!(cfg.config.profiles.haiku.is_default());
}
