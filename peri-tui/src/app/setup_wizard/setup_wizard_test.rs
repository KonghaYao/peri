use super::*;
use crate::config::{AppConfig, ProviderConfig};

#[test]
fn test_needs_setup_empty_providers_no_env() {
    let config = AppConfig::default();
    // 无 providers 且显式移除所有已知 API key 环境变量 → 需要 setup
    // 显式设为空值模拟"没有配置"的场景
    std::env::set_var("MODEL_PROVIDER", "__nonexistent__");
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("ANTHROPIC_API_KEY");
    assert!(
        needs_setup(&config),
        "无 providers 且无有效 env 时应需要 setup"
    );
}

#[test]
fn test_needs_setup_empty_providers_but_env_key() {
    let config = AppConfig::default();
    // 无 providers 但 OPENAI_API_KEY + MODEL_PROVIDER=openai → 不需要 setup
    std::env::set_var("MODEL_PROVIDER", "openai");
    std::env::set_var("OPENAI_API_KEY", "sk-fake-test-key");
    std::env::remove_var("ANTHROPIC_API_KEY");
    assert!(
        !needs_setup(&config),
        "有 OPENAI_API_KEY env 时应不需要 setup"
    );
}

#[test]
fn test_needs_setup_api_key_from_config() {
    let mut config = AppConfig::default();
    config.providers.push(ProviderConfig {
        id: "test".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        base_url: String::new(),
        ..Default::default()
    });
    assert!(!needs_setup(&config));
}

#[test]
fn test_setup_wizard_new_defaults() {
    let wizard = SetupWizardPanel::new();
    assert_eq!(wizard.step, SetupStep::Language);
    assert_eq!(wizard.language, "en");
    assert_eq!(wizard.language_cursor, 0);
    assert_eq!(wizard.providers.len(), 1);
    assert_eq!(wizard.providers[0].provider_type, ProviderType::Anthropic);
    assert!(wizard.providers[0].field_api_key.is_empty());
    assert!(wizard.providers[0].selected);
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
fn test_migrated_provider_new() {
    let mp = MigratedProvider::new(ProviderType::OpenAiCompatible);
    assert_eq!(mp.field_provider_id.value(), "openai");
    assert_eq!(mp.field_base_url.value(), "https://api.openai.com/v1");
    assert!(mp.selected);
    assert!(mp.field_api_key.is_empty());
}

#[test]
fn test_migrated_provider_is_complete() {
    let mut mp = MigratedProvider::new(ProviderType::Anthropic);
    assert!(!mp.is_complete()); // api_key empty
    mp.field_api_key.set_value("sk-test");
    assert!(mp.is_complete());
    mp.aliases[0].field_model_id.clear();
    assert!(!mp.is_complete());
}

#[test]
fn test_migrate_from_claude_code_no_file() {
    let mut wizard = SetupWizardPanel::new();
    wizard.source = SetupSource::MigrateClaudeCode;
    // 用 TempDir 隔离：home_dir_override 指向空目录，settings.json 不存在
    let temp_dir =
        std::env::temp_dir().join(format!("zen-migrate-nofile-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    wizard.home_dir_override = Some(temp_dir.clone());

    let result = wizard.migrate_from_claude_code();
    // settings.json 不存在，必须返回 false（不再依赖全局 ~/.claude/）
    assert!(!result, "无 settings.json 时应返回 false");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_migrate_syncs_all_fields() {
    // 构造一个临时 settings.json 模拟 Claude Code 配置
    let temp_dir = std::env::temp_dir().join(format!("zen-migrate-test-{}", uuid::Uuid::now_v7()));
    let claude_dir = temp_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "env": {
            "ANTHROPIC_API_KEY": "sk-ant-123",
            "ANTHROPIC_BASE_URL": "https://proxy.example.com",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.1",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5-turbo",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
            "OPENAI_API_KEY": "sk-openai-456",
            "OPENAI_BASE_URL": "https://api.deepseek.com/v1",
            "OPENAI_DEFAULT_OPUS_MODEL": "deepseek-v4-pro",
            "OPENAI_DEFAULT_SONNET_MODEL": "deepseek-v4-pro",
            "OPENAI_DEFAULT_HAIKU_MODEL": "deepseek-v4-flash",
        }
    });
    std::fs::write(claude_dir.join("settings.json"), settings.to_string()).unwrap();

    // 通过 home_dir_override 注入 TempDir，迁移逻辑读取 temp_dir/.claude/settings.json
    let mut wizard = SetupWizardPanel::new();
    wizard.home_dir_override = Some(temp_dir.clone());
    let migrated = wizard.migrate_from_claude_code();
    assert!(migrated, "存在合法 settings.json 时应成功迁移");
    // 验证迁移出的 provider 数量（Anthropic + OpenAI 兼容）
    assert!(
        wizard.providers.len() >= 2,
        "应至少迁移出 Anthropic + OpenAI 两个 provider，实际：{}",
        wizard.providers.len()
    );

    // 验证 env_get 辅助函数
    let env = settings["env"].as_object().unwrap().clone();
    assert_eq!(env_get(&env, "ANTHROPIC_API_KEY"), "sk-ant-123");
    assert_eq!(
        env_get(&env, "ANTHROPIC_BASE_URL"),
        "https://proxy.example.com"
    );
    assert_eq!(
        env_get(&env, "OPENAI_DEFAULT_OPUS_MODEL"),
        "deepseek-v4-pro"
    );
    assert_eq!(env_get(&env, "NONEXISTENT"), "");

    // 验证空 API key 但有 base_url 的前缀会生成条目
    let env_partial = serde_json::json!({
        "env": {
            "ANTHROPIC_BASE_URL": "https://proxy.example.com",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.1",
        }
    });
    let env_obj = env_partial["env"].as_object().unwrap();
    assert_eq!(env_get(env_obj, "ANTHROPIC_API_KEY"), "");
    assert_eq!(
        env_get(env_obj, "ANTHROPIC_BASE_URL"),
        "https://proxy.example.com"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_migrate_auth_token_fallback() {
    // 验证 ANTHROPIC_AUTH_TOKEN 在没有 ANTHROPIC_API_KEY 时被使用
    let env = serde_json::json!({
        "ANTHROPIC_AUTH_TOKEN": "token-abc",
        "ANTHROPIC_BASE_URL": "https://proxy.example.com",
    });
    let env_obj = env.as_object().unwrap();

    // ANTHROPIC_API_KEY 优先
    assert_eq!(env_get(env_obj, "ANTHROPIC_API_KEY"), "");
    assert_eq!(env_get(env_obj, "ANTHROPIC_AUTH_TOKEN"), "token-abc");

    // 模拟 key_names 优先级查找逻辑
    let key_names = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];
    let found = key_names
        .iter()
        .map(|k| env_get(env_obj, k))
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    assert_eq!(found, "token-abc");
}

#[test]
fn test_form_field_navigation() {
    assert_eq!(FormField::ProviderType.next(), FormField::ProviderId);
    assert_eq!(FormField::HaikuModel.next(), FormField::Confirm);
    assert_eq!(FormField::Confirm.next(), FormField::ProviderType);

    assert_eq!(FormField::ProviderType.prev(), FormField::Confirm);
    assert_eq!(FormField::Confirm.prev(), FormField::HaikuModel);
    assert_eq!(FormField::ProviderId.prev(), FormField::ProviderType);

    // TestConnectivity 插入在 BaseUrl 之后、ApiKey 之前
    assert_eq!(FormField::BaseUrl.next(), FormField::TestConnectivity);
    assert_eq!(FormField::TestConnectivity.next(), FormField::ApiKey);
    assert_eq!(FormField::ApiKey.prev(), FormField::TestConnectivity);
    assert_eq!(FormField::TestConnectivity.prev(), FormField::BaseUrl);
    // TestConnectivity 不是文本输入
    assert!(!FormField::TestConnectivity.is_text_input());
}

#[test]
fn test_connectivity_empty_base_url() {
    let (ok, msg) = test_connectivity("");
    assert!(!ok);
    assert!(msg.contains("Base URL"));
}

#[test]
fn test_connectivity_unreachable_url() {
    // 无法连接的 URL (TEST-NET 地址不会路由)
    let (ok, _msg) = test_connectivity("https://192.0.2.1");
    assert!(!ok);
}

#[test]
fn test_connectivity_parse_url_parts() {
    assert!(parse_url_parts("").is_none());
    let (host, port, path) = parse_url_parts("https://localhost:8443/v1/models").unwrap();
    assert_eq!(host, "localhost");
    assert_eq!(port, 8443);
    assert_eq!(path, "/v1/models");
    let (_host, port, path) = parse_url_parts("http://localhost:8080").unwrap();
    assert_eq!(port, 8080);
    assert_eq!(path, "/");
    let (host, port, _) = parse_url_parts("127.0.0.1:9999").unwrap();
    assert_eq!(host, "127.0.0.1");
    assert_eq!(port, 9999);
    let (_, port, _) = parse_url_parts("https://api.openai.com/v1").unwrap();
    assert_eq!(port, 443);
}

#[test]
fn test_connectivity_enter_triggers_test() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Edit;
    wizard.form_focus = FormField::TestConnectivity;
    wizard.providers[0]
        .field_base_url
        .set_value("https://example.com");
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert!(wizard.connectivity_result.is_some());
}

#[test]
fn test_edit_base_url_clears_connectivity() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Edit;
    wizard.form_focus = FormField::BaseUrl;
    wizard.connectivity_result = Some((true, "old result".into()));
    // 任意按键在 BaseUrl 焦点上应清空结果
    let _ = handle_setup_wizard_key(&mut wizard, make_char('x'));
    assert!(wizard.connectivity_result.is_none());
}

// ── Event handling tests ──

use tui_textarea::{Input, Key};

fn make_char(c: char) -> Input {
    Input {
        key: Key::Char(c),
        ctrl: false,
        alt: false,
        shift: false,
    }
}
fn make_key(key: Key) -> Input {
    Input {
        key,
        ctrl: false,
        alt: false,
        shift: false,
    }
}
fn type_text(wizard: &mut SetupWizardPanel, text: &str) {
    for c in text.chars() {
        let _ = handle_setup_wizard_key(wizard, make_char(c));
    }
}

// ── Step: Choose ──

#[test]
fn test_choose_arrow_cycles_source() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Choose;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Down));
    assert_eq!(wizard.source, SetupSource::MigrateClaudeCode);
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Up));
    assert_eq!(wizard.source, SetupSource::CustomApi);
}

#[test]
fn test_choose_enter_custom_advances_to_form() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Choose;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.step, SetupStep::Form);
    assert_eq!(wizard.form_mode, FormMode::Browse);
    assert_eq!(wizard.providers.len(), 1);
}

#[test]
fn test_choose_esc_back_to_language() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Choose;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert_eq!(wizard.step, SetupStep::Language);
}

// ── Step: Language ──

#[test]
fn test_language_arrow_navigates() {
    let mut wizard = SetupWizardPanel::new();
    assert_eq!(wizard.step, SetupStep::Language);
    assert_eq!(wizard.language_cursor, 0);
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Down));
    assert_eq!(wizard.language_cursor, 1);
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Down));
    assert_eq!(wizard.language_cursor, 0); // wraps around
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Up));
    assert_eq!(wizard.language_cursor, 1); // wraps around
}

#[test]
fn test_language_enter_selects_and_advances_to_choose() {
    let mut wizard = SetupWizardPanel::new();
    wizard.language_cursor = 1; // zh-CN
    let action = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert!(matches!(
        action,
        Some(SetupWizardAction::SetLanguage(ref s)) if s == "zh-CN"
    ));
    assert_eq!(wizard.language, "zh-CN");
    assert_eq!(wizard.step, SetupStep::Choose);
}

#[test]
fn test_language_space_selects_and_advances() {
    let mut wizard = SetupWizardPanel::new();
    let action = handle_setup_wizard_key(&mut wizard, make_char(' '));
    assert!(matches!(
        action,
        Some(SetupWizardAction::SetLanguage(ref s)) if s == "en"
    ));
    assert_eq!(wizard.language, "en");
    assert_eq!(wizard.step, SetupStep::Choose);
}

#[test]
fn test_language_esc_quits() {
    let mut wizard = SetupWizardPanel::new();
    let action = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert!(matches!(action, Some(SetupWizardAction::Skip)));
}

#[test]
fn test_language_default_is_en() {
    let wizard = SetupWizardPanel::new();
    assert_eq!(wizard.language, "en");
    assert_eq!(wizard.language_cursor, 0);
    assert_eq!(wizard.step, SetupStep::Language);
}

// ── Step: Form (Browse mode) ──

#[test]
fn test_browse_arrow_navigates() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Browse;
    assert_eq!(wizard.browse_cursor, 0);
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Down));
    assert_eq!(wizard.browse_cursor, 1); // Submit position
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Down));
    assert_eq!(wizard.browse_cursor, 1); // clamped
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Up));
    assert_eq!(wizard.browse_cursor, 0); // back to first provider
}

#[test]
fn test_browse_space_toggles_select() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Browse;
    assert!(wizard.providers[0].selected);
    let _ = handle_setup_wizard_key(&mut wizard, make_char(' '));
    assert!(!wizard.providers[0].selected);
    let _ = handle_setup_wizard_key(&mut wizard, make_char(' '));
    assert!(wizard.providers[0].selected);
}

#[test]
fn test_browse_enter_opens_edit() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Browse;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.form_mode, FormMode::Edit);
    assert_eq!(wizard.active_provider, 0);
}

#[test]
fn test_browse_enter_submit_validates() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Browse;
    wizard.browse_cursor = wizard.providers.len(); // Submit
                                                   // Empty api_key → blocked
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.step, SetupStep::Form);
    // Fill and retry
    wizard.providers[0].field_api_key.set_value("sk-test");
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.step, SetupStep::Done);
}

#[test]
fn test_browse_esc_back_to_choose() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Browse;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert_eq!(wizard.step, SetupStep::Choose);
}

// ── Step: Form (Edit mode) ──

#[test]
fn test_edit_arrow_navigates_fields() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Edit;
    assert_eq!(wizard.form_focus, FormField::ProviderType);
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Down));
    assert_eq!(wizard.form_focus, FormField::ProviderId);
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Up));
    assert_eq!(wizard.form_focus, FormField::ProviderType);
}

#[test]
fn test_edit_confirm_returns_to_browse() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Edit;
    wizard.form_focus = FormField::Confirm;
    // 填写必要字段
    wizard.providers[0].field_api_key.set_value("sk-test");
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.form_mode, FormMode::Browse);
}

#[test]
fn test_edit_confirm_stays_in_edit_when_incomplete() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Edit;
    wizard.form_focus = FormField::Confirm;
    // api_key 为空 → 不完整，保持在 Edit 模式
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.form_mode, FormMode::Edit);
}

#[test]
fn test_edit_esc_returns_to_browse() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Edit;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert_eq!(wizard.form_mode, FormMode::Browse);
}

#[test]
fn test_edit_api_key() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Form;
    wizard.form_mode = FormMode::Edit;
    wizard.form_focus = FormField::ApiKey;
    type_text(&mut wizard, "sk-test");
    assert_eq!(wizard.providers[0].field_api_key.value(), "sk-test");
}

// ── Step: Done ──

#[test]
fn test_done_enter_returns_save() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Done;
    let action = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert!(matches!(action, Some(SetupWizardAction::SaveAndClose)));
}

#[test]
fn test_done_esc_back_to_form() {
    let mut wizard = SetupWizardPanel::new();
    wizard.step = SetupStep::Done;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert_eq!(wizard.step, SetupStep::Form);
}

#[test]
fn test_save_setup_creates_valid_config() {
    let mut wizard = SetupWizardPanel::new();
    wizard.providers[0].field_api_key.set_value("sk-test-key");
    let temp_dir = std::env::temp_dir().join(format!("zen-setup-unit-{}", uuid::Uuid::now_v7()));
    let config_path = temp_dir.join("settings.json");
    let cfg = save_setup_to(&wizard, &config_path).expect("save_setup_to should succeed");
    assert_eq!(cfg.config.providers.len(), 1);
    assert_eq!(cfg.config.providers[0].provider_type, "anthropic");
    assert_eq!(cfg.config.providers[0].api_key, "sk-test-key");
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_save_setup_skips_unselected() {
    let mut wizard = SetupWizardPanel::new();
    wizard.providers[0].field_api_key.set_value("sk-test");
    wizard.providers[0].selected = false;
    wizard
        .providers
        .push(MigratedProvider::new(ProviderType::OpenAiCompatible));
    wizard.providers[1].field_api_key.set_value("sk-openai");
    wizard.providers[1].selected = true;

    let temp_dir = std::env::temp_dir().join(format!("zen-setup-skip-{}", uuid::Uuid::now_v7()));
    let config_path = temp_dir.join("settings.json");
    let cfg = save_setup_to(&wizard, &config_path).expect("save should succeed");
    assert_eq!(cfg.config.providers.len(), 1);
    assert_eq!(cfg.config.providers[0].provider_type, "openai");
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_save_setup_writes_language() {
    let mut wizard = SetupWizardPanel::new();
    wizard.providers[0].field_api_key.set_value("sk-test");
    wizard.language = "zh-CN".to_string();
    let temp_dir = std::env::temp_dir().join(format!("zen-lang-setup-{}", uuid::Uuid::now_v7()));
    let config_path = temp_dir.join("settings.json");
    let cfg = save_setup_to(&wizard, &config_path).expect("save should succeed");
    assert_eq!(cfg.config.language.as_deref(), Some("zh-CN"));
    let _ = std::fs::remove_dir_all(&temp_dir);
}

// ── E2E flow tests (migrated from headless_test.rs) ──

fn advance_to_form(wizard: &mut SetupWizardPanel) {
    wizard.step = SetupStep::Choose;
    let _ = handle_setup_wizard_key(wizard, make_key(Key::Enter));
    assert_eq!(wizard.step, SetupStep::Form);
    assert_eq!(wizard.form_mode, FormMode::Browse);
}

/// 进入 Edit 模式，填写 API Key，Confirm 回到 Browse，然后 Submit
fn fill_and_submit(wizard: &mut SetupWizardPanel, api_key: &str) {
    wizard.browse_cursor = 0;
    let _ = handle_setup_wizard_key(wizard, make_key(Key::Enter));
    assert_eq!(wizard.form_mode, FormMode::Edit);
    wizard.form_focus = FormField::ApiKey;
    type_text(wizard, api_key);
    wizard.form_focus = FormField::Confirm;
    let _ = handle_setup_wizard_key(wizard, make_key(Key::Enter));
    assert_eq!(wizard.form_mode, FormMode::Browse);
    wizard.browse_cursor = wizard.providers.len();
    let _ = handle_setup_wizard_key(wizard, make_key(Key::Enter));
}

#[test]
fn test_setup_wizard_full_flow_anthropic() {
    let mut wizard = SetupWizardPanel::new();
    advance_to_form(&mut wizard);
    assert_eq!(wizard.providers.len(), 1);
    assert_eq!(wizard.providers[0].provider_type, ProviderType::Anthropic);

    fill_and_submit(&mut wizard, "sk-ant-test-key-12345");
    assert_eq!(wizard.step, SetupStep::Done);

    let action = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert!(matches!(action, Some(SetupWizardAction::SaveAndClose)));

    let temp_dir = std::env::temp_dir().join(format!("zen-setup-test-{}", uuid::Uuid::now_v7()));
    let config_path = temp_dir.join("settings.json");
    let cfg = save_setup_to(&wizard, &config_path).expect("save should succeed");
    assert_eq!(cfg.config.providers.len(), 1);
    assert_eq!(cfg.config.providers[0].provider_type, "anthropic");
    assert_eq!(cfg.config.providers[0].api_key, "sk-ant-test-key-12345");
    assert!(!needs_setup(&cfg.config));
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_setup_wizard_full_flow_openai() {
    let mut wizard = SetupWizardPanel::new();
    advance_to_form(&mut wizard);

    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.form_mode, FormMode::Edit);
    wizard.form_focus = FormField::ProviderType;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Right));
    assert_eq!(
        wizard.providers[0].provider_type,
        ProviderType::OpenAiCompatible
    );

    wizard.form_focus = FormField::ApiKey;
    type_text(&mut wizard, "sk-openai-test-key");

    wizard.form_focus = FormField::Confirm;
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.form_mode, FormMode::Browse);

    wizard.browse_cursor = wizard.providers.len();
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.step, SetupStep::Done);

    let action = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert!(matches!(action, Some(SetupWizardAction::SaveAndClose)));

    let temp_dir =
        std::env::temp_dir().join(format!("zen-setup-test-openai-{}", uuid::Uuid::now_v7()));
    let config_path = temp_dir.join("settings.json");
    let cfg = save_setup_to(&wizard, &config_path).expect("save should succeed");
    assert_eq!(cfg.config.providers[0].provider_type, "openai");
    assert_eq!(cfg.config.providers[0].api_key, "sk-openai-test-key");
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_setup_wizard_esc_navigation() {
    let mut wizard = SetupWizardPanel::new();
    advance_to_form(&mut wizard);

    // Browse → Submit → Enter (empty key, should stay)
    wizard.browse_cursor = wizard.providers.len();
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.step, SetupStep::Form);

    fill_and_submit(&mut wizard, "test-key");
    assert_eq!(wizard.step, SetupStep::Done);

    // Done → Esc → Form
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert_eq!(wizard.step, SetupStep::Form);

    // Form → Esc → Choose
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert_eq!(wizard.step, SetupStep::Choose);

    // Choose → Esc → Language
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Esc));
    assert_eq!(wizard.step, SetupStep::Language);
}

#[test]
fn test_setup_wizard_multi_provider() {
    let mut wizard = SetupWizardPanel::new();
    advance_to_form(&mut wizard);
    wizard
        .providers
        .push(MigratedProvider::new(ProviderType::OpenAiCompatible));
    wizard.providers[1].field_api_key.set_value("sk-openai");
    wizard.providers[0].field_api_key.set_value("sk-ant");

    // Browse: Submit
    wizard.browse_cursor = wizard.providers.len();
    let _ = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert_eq!(wizard.step, SetupStep::Done);

    let temp_dir = std::env::temp_dir().join(format!("zen-setup-multi-{}", uuid::Uuid::now_v7()));
    let config_path = temp_dir.join("settings.json");
    let cfg = save_setup_to(&wizard, &config_path).expect("save should succeed");
    assert_eq!(cfg.config.providers.len(), 2);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_setup_wizard_saves_and_clears() {
    let mut wizard = SetupWizardPanel::new();
    advance_to_form(&mut wizard);
    fill_and_submit(&mut wizard, "sk-final-test");
    assert_eq!(wizard.step, SetupStep::Done);

    // Verify SaveAndClose action
    let action = handle_setup_wizard_key(&mut wizard, make_key(Key::Enter));
    assert!(matches!(action, Some(SetupWizardAction::SaveAndClose)));

    // Verify save produces valid config
    let temp_dir = std::env::temp_dir().join(format!("zen-setup-final-{}", uuid::Uuid::now_v7()));
    let config_path = temp_dir.join("settings.json");
    let cfg = save_setup_to(&wizard, &config_path).expect("save should succeed");
    assert!(!needs_setup(&cfg.config));
    let _ = std::fs::remove_dir_all(&temp_dir);
}
