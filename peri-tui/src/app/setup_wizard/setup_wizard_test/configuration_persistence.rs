//! Configuration persistence regression scenarios.

use super::FormMode;
use super::PROVIDER_PRESETS;
use super::PathBuf;
use super::SetupSource;
use super::SetupStep;
use super::SetupWizardState;

/// [回归测试] 重开表单后保存仅编辑 provider 字段，保留用户的档位和扩展配置。
#[test]
fn test_reopened_setup_preserves_active_profile_and_provider_metadata() {
    let mut cfg = super::build_wizard_config(&SetupWizardState {
        providers: vec![PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1].to_migrated()],
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
    let profiles = serde_json::to_value(&cfg.config.profiles).expect("reopened setup preserves active profile and provider metadata: fixture must serialize to JSON");
    let mut draft = super::state_from_config(&cfg);
    assert!(draft.from_command);
    assert_eq!(draft.step, SetupStep::Form);
    assert_eq!(draft.source, SetupSource::CustomApi);
    assert_eq!(draft.form_mode, FormMode::Browse);
    draft.providers[0].base_url = "http://localhost:7001/v1".into();
    let merged = super::merge_setup(&draft, cfg);
    assert_eq!(merged.config.active_alias, "haiku");
    assert_eq!(
        serde_json::to_value(&merged.config.profiles).expect("reopened setup preserves active profile and provider metadata: fixture data must serialize to JSON"),
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

/// [回归测试] 已有自定义 provider 时重开 `/setup` 并保存，不得用默认值覆盖，
/// 也不得新增同 ID 的重复 provider；真实 HOME/仓库配置只由父进程做前后比对。
#[test]
fn test_reopened_setup_save_preserves_existing_custom_provider_fields() {
    isolated(
        "test_reopened_setup_save_preserves_existing_custom_provider_fields",
        || {
            let root = std::env::current_dir().expect("reopened setup save preserves existing custom provider fields: isolated fixture workspace must remain accessible");
            let home = PathBuf::from(std::env::var_os("HOME").expect("reopened setup save preserves existing custom provider fields: the isolated fixture environment must contain its configured value"));
            let cwd = root.join("workspace");
            std::fs::create_dir_all(cwd.join(".peri")).expect("reopened setup save preserves existing custom provider fields: isolated fixture workspace must remain accessible");
            let global = home.join(".peri/settings.json");
            let cfg: crate::config::PeriConfig = serde_json::from_value(serde_json::json!({
                "config": {
                    "active_alias": "opus",
                    "language": "zh-CN",
                    "profiles": {
                        "opus": {
                            "provider": "custom",
                            "model": "keep-model",
                            "effort": "high"
                        }
                    },
                    "providers": [{
                        "id": "custom",
                        "name": "Keep me",
                        "type": "openai",
                        "apiKey": "custom-key",
                        "baseUrl": "https://custom.example/v1",
                        "models": {
                            "fable": "fable-keep",
                            "opus": "opus-keep",
                            "sonnet": "sonnet-keep",
                            "haiku": "haiku-keep"
                        }
                    }]
                }
            }))
            .expect("reopened setup save preserves existing custom provider fields: fixture JSON must match the expected schema");
            crate::config::save_to(&cfg, &global).expect("reopened setup save preserves existing custom provider fields: the save_to fixture operation uses valid inputs");
            let global_before = std::fs::read(&global).expect("reopened setup save preserves existing custom provider fields: isolated fixture workspace must remain accessible");
            std::fs::write(
                cwd.join(".peri/settings.json"),
                r#"{"config":{"language":"zh-CN"}}"#,
            )
            .expect("reopened setup save preserves existing custom provider fields: isolated fixture workspace must remain accessible");
            let sentinel = root.join(".peri/settings.json");
            std::fs::create_dir_all(sentinel.parent().expect("reopened setup save preserves existing custom provider fields: the fixture path must have its expected parent directory")).expect("reopened setup save preserves existing custom provider fields: isolated fixture workspace must remain accessible");
            std::fs::write(&sentinel, "do-not-touch-sentinel").expect("reopened setup save preserves existing custom provider fields: isolated fixture workspace must remain accessible");

            let source = std::sync::Arc::new(
                crate::config::ConfigSource::load_at(&cwd, global.clone()).expect("reopened setup save preserves existing custom provider fields: the load_at fixture operation uses valid inputs"),
            );
            assert!(
                crate::kit::atoms::CONFIG_SOURCE_HANDLE
                    .set(source.clone())
                    .is_ok()
            );

            let state = super::state_from_config(&source.loaded_merged());
            assert_eq!(state.step, SetupStep::Form, "已有 provider 直接进入 Form");
            assert_eq!(state.providers.len(), 1);
            assert_eq!(state.providers[0].provider_id, "custom");

            let merged = super::save_setup(&state).expect("reopened setup save preserves existing custom provider fields: the save_setup fixture operation uses valid inputs");
            assert_eq!(merged.config.providers.len(), 1, "不得新增重复 provider");
            let disk = crate::config::ConfigSource::load_at(&cwd, global.clone())
                .expect("reopened setup save preserves existing custom provider fields: the load_at fixture operation uses valid inputs")
                .loaded_merged();
            assert_eq!(
                merged, disk,
                "save_setup 必须返回 authority 接纳后的快照，而不是保存前候选"
            );
            assert_eq!(disk.config.providers.len(), 1);
            let provider = disk
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "custom")
                .expect("既有 provider 必须保留");
            assert_eq!(provider.provider_type, "openai");
            assert_eq!(provider.api_key, "custom-key");
            assert_eq!(provider.base_url, "https://custom.example/v1");
            assert_eq!(provider.models.fable, "fable-keep");
            assert_eq!(provider.models.opus, "opus-keep");
            assert_eq!(provider.models.sonnet, "sonnet-keep");
            assert_eq!(provider.models.haiku, "haiku-keep");
            assert_eq!(provider.name.as_deref(), Some("Keep me"));
            assert_eq!(disk.config.profiles.opus.provider, "custom");
            assert_eq!(
                disk.config.profiles.opus.model.as_deref(),
                Some("keep-model")
            );
            assert_eq!(std::fs::read(&global).expect("reopened setup save preserves existing custom provider fields: the fixture storage and layout must support the requested operation"), global_before);
            assert_eq!(
                std::fs::read_to_string(&sentinel).expect("reopened setup save preserves existing custom provider fields: the fixture storage and layout must support the requested operation"),
                "do-not-touch-sentinel"
            );
        },
    );
}

/// [回归测试] 已有 `openai_responses` provider（含 apiKey/auth/endpoint/models）
/// 重开 `/setup` 保存时，表单投影不得把原始协议或认证引用改写为 `openai`。
#[test]
fn test_reopened_setup_preserves_openai_responses_provider() {
    isolated(
        "test_reopened_setup_preserves_openai_responses_provider",
        || {
            let root = std::env::current_dir().expect("reopened setup preserves openai responses provider: isolated fixture workspace must remain accessible");
            let home = PathBuf::from(std::env::var_os("HOME").expect("reopened setup preserves openai responses provider: the isolated fixture environment must contain its configured value"));
            let cwd = root.join("workspace");
            std::fs::create_dir_all(cwd.join(".peri")).expect("reopened setup preserves openai responses provider: isolated fixture workspace must remain accessible");
            let global = home.join(".peri/settings.json");
            let responses_url = "https://api.openai.com/v1/responses";
            let cfg: crate::config::PeriConfig = serde_json::from_value(serde_json::json!({
                "config": {
                    "active_alias": "opus",
                    "profiles": {
                        "opus": {
                            "provider": "codex",
                            "model": "gpt-6-astra",
                            "contextWindow": 64000
                        }
                    },
                    "providers": [{
                        "id": "codex",
                        "name": "Keep Responses",
                        "type": "openai_responses",
                        "apiKey": "responses-key",
                        "baseUrl": responses_url,
                        "auth": {"kind": "api_key"},
                        "models": {
                            "fable": "gpt-6-astra",
                            "opus": "gpt-6-astra",
                            "sonnet": "gpt-6.1-sol",
                            "haiku": "gpt-6-luna"
                        }
                    }]
                }
            }))
            .expect("reopened setup preserves openai responses provider: fixture JSON must match the expected schema");
            crate::config::save_to(&cfg, &global).expect("reopened setup preserves openai responses provider: the save_to fixture operation uses valid inputs");
            let global_before = std::fs::read(&global).expect("reopened setup preserves openai responses provider: isolated fixture workspace must remain accessible");
            std::fs::write(
                cwd.join(".peri/settings.json"),
                r#"{"config":{"language":"en"}}"#,
            )
            .expect("reopened setup preserves openai responses provider: isolated fixture workspace must remain accessible");
            let sentinel = root.join(".peri/settings.json");
            std::fs::create_dir_all(sentinel.parent().expect("reopened setup preserves openai responses provider: the fixture path must have its expected parent directory")).expect("reopened setup preserves openai responses provider: isolated fixture workspace must remain accessible");
            std::fs::write(&sentinel, "do-not-touch-sentinel").expect("reopened setup preserves openai responses provider: isolated fixture workspace must remain accessible");

            let source = std::sync::Arc::new(
                crate::config::ConfigSource::load_at(&cwd, global.clone()).expect("reopened setup preserves openai responses provider: the load_at fixture operation uses valid inputs"),
            );
            assert!(
                crate::kit::atoms::CONFIG_SOURCE_HANDLE
                    .set(source.clone())
                    .is_ok()
            );

            let state = super::state_from_config(&source.loaded_merged());
            assert_eq!(state.step, SetupStep::Form, "已有 provider 直接进入 Form");
            assert_eq!(state.providers.len(), 1);
            assert_eq!(state.providers[0].provider_id, "codex");

            let merged = super::save_setup(&state).expect("既有 Responses provider 必须可保存");
            let provider = merged
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "codex")
                .expect("既有 provider 必须保留");
            assert_eq!(provider.provider_type, "openai_responses");
            assert_eq!(
                serde_json::to_value(&provider.auth).expect("reopened setup preserves openai responses provider: fixture data must serialize to JSON"),
                serde_json::json!({"kind": "api_key"})
            );
            assert_eq!(provider.api_key, "responses-key");
            assert_eq!(provider.base_url, responses_url);
            assert_eq!(provider.models.fable, "gpt-6-astra");
            assert_eq!(provider.models.opus, "gpt-6-astra");
            assert_eq!(provider.models.sonnet, "gpt-6.1-sol");
            assert_eq!(provider.models.haiku, "gpt-6-luna");
            assert_eq!(provider.name.as_deref(), Some("Keep Responses"));

            let disk = crate::config::ConfigSource::load_at(&cwd, global.clone())
                .expect("reopened setup preserves openai responses provider: the load_at fixture operation uses valid inputs")
                .loaded_merged();
            let provider = disk
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "codex")
                .expect("落盘后 provider 必须保留");
            assert_eq!(provider.provider_type, "openai_responses");
            assert_eq!(
                serde_json::to_value(&provider.auth).expect("reopened setup preserves openai responses provider: fixture data must serialize to JSON"),
                serde_json::json!({"kind": "api_key"})
            );
            assert_eq!(provider.base_url, responses_url);
            assert_eq!(provider.models.haiku, "gpt-6-luna");
            assert_eq!(std::fs::read(&global).expect("reopened setup preserves openai responses provider: the fixture storage and layout must support the requested operation"), global_before);
            assert_eq!(
                std::fs::read_to_string(&sentinel).expect("reopened setup preserves openai responses provider: the fixture storage and layout must support the requested operation"),
                "do-not-touch-sentinel"
            );
        },
    );
}

/// [回归测试] 保存失败时 finish_setup 不得发布保存前候选，也不得完成 setup。
#[test]
fn test_finish_setup_never_publishes_credential_stripped_candidate() {
    isolated(
        "test_finish_setup_never_publishes_credential_stripped_candidate",
        || {
            let (cwd, global, workspace, global_before) = super::write_inherited_key_fixture();
            let workspace_before = std::fs::read(&workspace).expect("finish setup never publishes credential stripped candidate: isolated fixture workspace must remain accessible");
            let source = std::sync::Arc::new(
                crate::config::ConfigSource::load_at(&cwd, global.clone()).expect("finish setup never publishes credential stripped candidate: the load_at fixture operation uses valid inputs"),
            );
            assert!(
                crate::kit::atoms::CONFIG_SOURCE_HANDLE
                    .set(source.clone())
                    .is_ok()
            );
            let original = source.loaded_merged();
            let handle = std::sync::Arc::new(parking_lot::RwLock::new(original.clone()));
            assert!(
                crate::kit::atoms::PERI_CONFIG_HANDLE
                    .set(handle.clone())
                    .is_ok()
            );

            let mut state = super::state_from_config(&original);
            state.from_command = true;
            state.save_in_progress = true;
            state.providers[0].base_url = "https://changed.example/v1".into();
            SETUP_WIZARD.set(state.clone());

            let runtime = tokio::runtime::Runtime::new().expect("finish setup never publishes credential stripped candidate: fixture parameters must construct a valid value");
            runtime.block_on(crate::kit::setup_wizard::finish_setup(state));

            assert_eq!(
                *handle.read(),
                original,
                "失败时运行态必须保持已持久化配置，不得发布候选"
            );
            assert!(!*crate::kit::atoms::SETUP_COMPLETED.state().read());
            let wizard = SETUP_WIZARD.state();
            let current = wizard.read();
            assert!(current.submit_error.is_some());
            assert!(!current.save_in_progress);
            drop(current);
            assert_eq!(std::fs::read(&workspace).expect("finish setup never publishes credential stripped candidate: the fixture storage and layout must support the requested operation"), workspace_before);
            assert_eq!(std::fs::read(&global).expect("finish setup never publishes credential stripped candidate: the fixture storage and layout must support the requested operation"), global_before);
            assert!(
                !std::fs::read_to_string(&workspace)
                    .expect("finish setup never publishes credential stripped candidate: the fixture storage and layout must support the requested operation")
                    .contains("global-secret")
            );
        },
    );
}

/// [回归测试] 保存成功时 finish_setup 发布 authority 接纳的 accepted snapshot。
#[test]
fn test_finish_setup_publishes_accepted_snapshot() {
    isolated("test_finish_setup_publishes_accepted_snapshot", || {
        let (cwd, global, workspace, global_before) = super::write_inherited_key_fixture();
        let source = std::sync::Arc::new(
            crate::config::ConfigSource::load_at(&cwd, global.clone()).expect("finish setup publishes accepted snapshot: the load_at fixture operation uses valid inputs"),
        );
        assert!(
            crate::kit::atoms::CONFIG_SOURCE_HANDLE
                .set(source.clone())
                .is_ok()
        );
        let original = source.loaded_merged();
        let handle = std::sync::Arc::new(parking_lot::RwLock::new(original.clone()));
        assert!(
            crate::kit::atoms::PERI_CONFIG_HANDLE
                .set(handle.clone())
                .is_ok()
        );

        let mut state = super::state_from_config(&original);
        state.from_command = false;
        state.save_in_progress = true;
        SETUP_WIZARD.set(state.clone());

        let runtime = tokio::runtime::Runtime::new().expect("finish setup publishes accepted snapshot: fixture parameters must construct a valid value");
        runtime.block_on(crate::kit::setup_wizard::finish_setup(state));

        let accepted = crate::config::ConfigSource::load_at(&cwd, global.clone())
            .expect("finish setup publishes accepted snapshot: the load_at fixture operation uses valid inputs")
            .loaded_merged();
        assert_eq!(*handle.read(), accepted, "只能发布 accepted snapshot");
        assert!(*crate::kit::atoms::SETUP_COMPLETED.state().read());
        assert!(SETUP_WIZARD.state().read().submit_error.is_none());
        assert_eq!(std::fs::read(&global).expect("finish setup publishes accepted snapshot: the fixture storage and layout must support the requested operation"), global_before);
        assert!(
            !std::fs::read_to_string(&workspace)
                .expect("finish setup publishes accepted snapshot: the fixture storage and layout must support the requested operation")
                .contains("global-secret")
        );
    });
}

use crate::kit::atoms::SETUP_WIZARD;

fn isolated(name: &str, scenario: fn()) {
    let module = module_path!()
        .split_once("::")
        .expect("the test module has a crate prefix")
        .1;
    super::run_isolated(module, name, scenario);
}
