//! Tests for setup_wizard
use super::*;
use serial_test::serial;

#[test]
#[serial]
fn test_needs_setup_empty_providers_no_env() {
    let _env = EnvKeys::clear();
    let config = crate::config::AppConfig::default();
    assert!(
        needs_setup(&config),
        "无 providers 且无有效 env 时应需要 setup"
    );
}

#[test]
#[serial]
fn test_needs_setup_api_key_from_config() {
    let _env = EnvKeys::clear();
    let mut config = crate::config::AppConfig {
        active_alias: "sonnet".into(),
        ..Default::default()
    };
    config.profiles.sonnet.provider = "test".into();
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
fn test_peri_free_provider_fields() {
    let mp = peri_free_provider();
    assert_eq!(mp.provider_id, "peri");
    assert_eq!(mp.base_url, PERI_FREE_BASE_URL);
    assert_eq!(mp.api_key, "public");
    assert_eq!(mp.provider_type, ProviderType::OpenAiCompatible);
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

struct EnvKeys(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvKeys {
    fn clear() -> Self {
        let saved = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect::<Vec<_>>();
        for (name, _) in &saved {
            unsafe { std::env::remove_var(name) };
        }
        Self(saved)
    }
}

impl Drop for EnvKeys {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// [回归测试] setup 检测必须与实际 provider 构造使用相同的 active_alias。
#[test]
#[serial]
fn test_needs_setup_empty_alias_matches_runtime_resolution() {
    let _env = EnvKeys::clear();
    let mut cfg = build_wizard_config(&SetupWizardState {
        providers: vec![peri_free_provider()],
        ..Default::default()
    });
    cfg.config.active_alias.clear();
    assert!(crate::app::agent::LlmProvider::from_config(&cfg).is_none());
    assert!(needs_setup(&cfg.config));
}

/// [回归测试] 重开表单后保存仅编辑 provider 字段，保留用户的档位和扩展配置。
#[test]
fn test_reopened_setup_preserves_active_profile_and_provider_metadata() {
    let mut cfg = build_wizard_config(&SetupWizardState {
        providers: vec![peri_free_provider()],
        ..Default::default()
    });
    cfg.config.active_alias = "haiku".into();
    cfg.config.profiles.haiku.provider = "peri".into();
    cfg.config.profiles.haiku.model = Some("custom-model".into());
    cfg.config.profiles.haiku.effort = "low".into();
    cfg.config.profiles.haiku.max_tokens = 1234;
    cfg.config.providers[0].name = Some("My endpoint".into());
    cfg.config.providers[0]
        .extra
        .insert("custom-option".into(), serde_json::json!(true));
    cfg.config
        .extra
        .insert("unrelated-option".into(), serde_json::json!(42));
    let profiles = serde_json::to_value(&cfg.config.profiles).unwrap();
    let mut draft = state_from_config(&cfg);
    assert!(draft.from_command);
    assert_eq!(draft.step, SetupStep::Form);
    assert_eq!(draft.form_mode, FormMode::Browse);
    draft.providers[0].base_url = "http://localhost:7001/v1".into();
    let merged = merge_setup(&draft, cfg);
    assert_eq!(merged.config.active_alias, "haiku");
    assert_eq!(
        serde_json::to_value(&merged.config.profiles).unwrap(),
        profiles
    );
    assert_eq!(
        merged.config.providers[0].base_url,
        "http://localhost:7001/v1"
    );
    assert_eq!(
        merged.config.providers[0].name.as_deref(),
        Some("My endpoint")
    );
    assert_eq!(merged.config.providers[0].extra["custom-option"], true);
    assert_eq!(merged.config.extra["unrelated-option"], 42);
}

#[test]
fn test_wizard_replaces_broken_active_selection_with_usable_profile() {
    let mut cfg = crate::config::PeriConfig::default();
    cfg.config.active_alias = "missing".into();
    let draft = SetupWizardState {
        from_command: true,
        providers: vec![peri_free_provider()],
        ..Default::default()
    };
    let merged = merge_setup(&draft, cfg);
    assert!(crate::app::agent::LlmProvider::from_config(&merged).is_some());
    assert_eq!(merged.config.active_alias, "opus");
}

/// [回归测试] 删除、粘贴等所有文本修改均经过同一失效入口。
#[test]
fn test_edit_invalidates_pending_connectivity() {
    let mut state = SetupWizardState {
        connectivity_generation: 9,
        connectivity_in_progress: true,
        connectivity_result: Some((true, "old result".into())),
        form_focus: FormField::BaseUrl,
        ..Default::default()
    };
    state.set_active_field_value("http://localhost:7002".into());
    assert_eq!(state.connectivity_generation, 10);
    assert!(!state.connectivity_in_progress);
    assert!(state.connectivity_result.is_none());
}

#[tokio::test]
async fn test_connectivity_http_status_and_errors_do_not_expose_url_or_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (response, expected_success) in [
        ("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", true),
        (
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n",
            false,
        ),
        (
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n",
            false,
        ),
        ("not an HTTP response: private-body", false),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let received = stream.read(&mut request).await.unwrap();
            assert!(received > 0, "客户端应发送 HTTP 请求");
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let result =
            test_connectivity(&format!("http://{address}/v1?credential=private-query")).await;
        server.await.unwrap();
        assert_eq!(result.0, expected_success);
        assert!(!result.1.contains("private-query"));
        assert!(!result.1.contains("private-body"));
        assert!(!result.1.contains(&address.to_string()));
    }
}

#[tokio::test]
async fn test_connectivity_rejects_embedded_credentials_safely() {
    for url in [
        "http://user:private-password@localhost",
        "bad-url?key=private-query",
        "file:///private-path",
    ] {
        let (success, message) = test_connectivity(url).await;
        assert!(!success);
        assert!(!message.contains("private-"));
        assert!(!message.contains(url));
    }
}

/// [回归测试] 超时覆盖“TCP 已连接但 HTTP 永不响应”，避免后台请求永久存活。
#[tokio::test]
async fn test_connectivity_stalled_http_response_times_out() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let received = stream.read(&mut request).await.unwrap();
        assert!(received > 0, "客户端应发送 HTTP 请求");
        std::future::pending::<()>().await;
        drop(stream);
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(7),
        test_connectivity(&format!("http://{address}")),
    )
    .await;
    server.abort();
    let _ = server.await;
    let (success, message) = result.expect("HTTP 检查应在 5 秒内自行超时");
    assert!(!success);
    assert!(!message.contains(&address.to_string()));
}

/// [回归测试] 已持久化的 provider 切换协议不改身份，当前档位实际使用新协议。
#[test]
fn test_persisted_provider_type_change_preserves_identity_and_active_binding() {
    let mut provider = MigratedProvider::new(ProviderType::Anthropic);
    provider.api_key = "test-only".into();
    let cfg = build_wizard_config(&SetupWizardState {
        providers: vec![provider],
        ..Default::default()
    });
    let mut draft = state_from_config(&cfg);
    let provider = &mut draft.providers[0];
    provider.provider_type.cycle();
    provider.refresh_provider_defaults();
    assert_eq!(provider.provider_id, "anthropic");
    assert_eq!(
        provider.base_url,
        ProviderType::OpenAiCompatible.default_base_url()
    );
    let merged = merge_setup(&draft, cfg);
    assert_eq!(merged.config.providers.len(), 1);
    assert_eq!(merged.config.profiles.opus.provider, "anthropic");
    assert!(matches!(
        crate::app::agent::LlmProvider::from_config(&merged),
        Some(crate::app::agent::LlmProvider::OpenAi { .. })
    ));
}

#[test]
fn test_new_provider_type_change_keeps_matching_default_identity() {
    let mut provider = MigratedProvider::new(ProviderType::Anthropic);
    provider.provider_type.cycle();
    provider.refresh_provider_defaults();
    assert_eq!(provider.provider_id, "openai");
}

/// [回归测试] 向导不提供已有身份改名；手动输入不能留下孤立旧绑定。
#[test]
fn test_persisted_provider_id_cannot_be_edited_or_saved_as_another_id() {
    let cfg = build_wizard_config(&SetupWizardState {
        providers: vec![peri_free_provider()],
        ..Default::default()
    });
    let mut draft = state_from_config(&cfg);
    draft.form_focus = FormField::ProviderId;
    assert!(!draft.active_field_is_editable());
    draft.set_active_field_value("other".into());
    assert_eq!(draft.providers[0].provider_id, "peri");
    draft.providers[0].provider_id = "other".into();
    assert!(
        save_setup(&draft).is_err(),
        "非法改名应在任何磁盘读写前拒绝"
    );
}

#[test]
fn test_provider_identity_provenance_survives_state_roundtrip() {
    let cfg = build_wizard_config(&SetupWizardState {
        providers: vec![peri_free_provider()],
        ..Default::default()
    });
    let state = state_from_config(&cfg);
    let encoded = serde_json::to_value(state).unwrap();
    let restored: SetupWizardState = serde_json::from_value(encoded).unwrap();
    assert!(!restored.providers[0].provider_id_is_editable());
    let mut old_provider = serde_json::to_value(peri_free_provider()).unwrap();
    old_provider
        .as_object_mut()
        .unwrap()
        .remove("original_provider_id");
    let restored: MigratedProvider = serde_json::from_value(old_provider).unwrap();
    assert!(restored.provider_id_is_editable());
}
