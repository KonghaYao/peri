//! Tests for setup_wizard
use super::FormField;
use super::FormMode;
use super::MigratedProvider;
use super::PROVIDER_PRESETS;
use super::ProviderType;
use super::SetupSource;
use super::SetupStep;
use super::SetupWizardState;

use serial_test::serial;
use std::path::{Path, PathBuf};

const SETUP_CHILD: &str = "PERI_SETUP_REOPEN_CHILD";

fn read_if_exists(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}

/// 真实配置只做前后比对，测试本身永远不读写它们。
fn guarded_config_paths() -> [PathBuf; 2] {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("peri-tui 位于 workspace 内")
        .join(".peri/settings.json");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME 已设置")
        .join(".peri/settings.json");
    [repo, home]
}

/// 配置提交用例在独立子进程中冻结临时 HOME/cwd，父进程核对真实配置文件未变。
fn run_isolated(test_module: &str, name: &str, scenario: fn()) {
    if std::env::var(SETUP_CHILD).as_deref() == Ok(name) {
        let home = PathBuf::from(std::env::var_os("HOME").expect(
            "isolated: the isolated fixture environment must contain its configured value",
        ));
        let cwd = std::env::current_dir()
            .expect("isolated: isolated fixture workspace must remain accessible");
        assert!(
            home.canonicalize()
                .expect("isolated: the canonicalize fixture operation uses valid inputs")
                .starts_with(
                    cwd.canonicalize()
                        .expect("isolated: the canonicalize fixture operation uses valid inputs")
                ),
            "子进程 HOME 必须位于临时 cwd 内"
        );
        assert_ne!(cwd, Path::new(env!("CARGO_MANIFEST_DIR")));
        scenario();
        return;
    }

    let guarded = guarded_config_paths().map(|path| (read_if_exists(&path), path));
    let dir =
        tempfile::tempdir().expect("isolated: isolated fixture workspace must remain accessible");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)
        .expect("isolated: isolated fixture workspace must remain accessible");
    let output = std::process::Command::new(
        std::env::current_exe()
            .expect("isolated: isolated fixture workspace must remain accessible"),
    )
    .args([
        "--exact",
        &format!("{test_module}::{name}"),
        "--nocapture",
        "--test-threads=1",
    ])
    .env(SETUP_CHILD, name)
    .env_remove("OPENAI_API_KEY")
    .env_remove("ANTHROPIC_API_KEY")
    .env_remove("CODEX_API_KEY")
    .env_remove("ANTHROPIC_BASE_URL")
    .env_remove("OPENAI_BASE_URL")
    .env_remove("CODEX_BASE_URL")
    .env("HOME", &home)
    .env("USERPROFILE", &home)
    .current_dir(dir.path())
    .output()
    .expect("isolated: isolated fixture workspace must remain accessible");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    for (before, path) in guarded {
        assert_eq!(
            read_if_exists(&path),
            before,
            "真实配置被测试触碰：{}",
            path.display()
        );
    }
}

#[test]
#[serial]
fn test_needs_setup_empty_providers_no_env() {
    isolated("test_needs_setup_empty_providers_no_env", || {
        let config = crate::config::AppConfig::default();
        assert!(
            super::needs_setup(&config),
            "无 providers 且无有效 env 时应需要 setup"
        );
    });
}

#[test]
#[serial]
fn test_needs_setup_api_key_from_config() {
    isolated("test_needs_setup_api_key_from_config", || {
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
        assert!(!super::needs_setup(&config));
    });
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
    assert_eq!(super::mask_api_key("sk-short"), "••••••••");
    assert_eq!(
        super::mask_api_key("sk-ant-api03-very-long-key-here"),
        "sk-a••••here"
    );
}

#[test]
fn test_free_gateway_preset_fields() {
    let preset = &PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1];
    let mp = preset.to_migrated();
    assert_eq!(mp.provider_id, "peri");
    assert_eq!(mp.base_url, preset.base_url);
    assert_eq!(mp.api_key, "public");
    assert_eq!(mp.provider_type, ProviderType::OpenAiCompatible);
    assert_eq!(mp.aliases, preset.models.map(String::from));
    assert!(mp.selected, "免费服务应默认选中");
    assert!(mp.is_complete(), "免费服务配置应视为完整");
    assert!(mp.fixed_tiers, "免密钥网关需要固定档位");
}

#[test]
fn test_build_wizard_config_fixed_tier_profiles() {
    let preset = &PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1];
    let state = SetupWizardState {
        step: SetupStep::Form,
        providers: vec![preset.to_migrated()],
        language: "zh-CN".to_string(),
        ..Default::default()
    };
    let cfg = super::build_wizard_config(&state);
    assert_eq!(
        cfg.config.active_alias, "sonnet",
        "免费服务默认档位为 sonnet"
    );
    assert_eq!(cfg.config.providers.len(), 1);
    let p = &cfg.config.providers[0];
    assert_eq!(p.id, "peri");
    assert_eq!(p.base_url, preset.base_url);
    assert_eq!(p.api_key, "public");
    assert_eq!(
        [
            p.models.fable.as_str(),
            p.models.opus.as_str(),
            p.models.sonnet.as_str(),
            p.models.haiku.as_str()
        ],
        preset.models
    );
    assert_eq!(cfg.config.profiles.fable.effort, "max");
    assert_eq!(cfg.config.profiles.opus.effort, "medium");
    assert_eq!(cfg.config.profiles.sonnet.effort, "max");
    assert_eq!(cfg.config.profiles.haiku.effort, "low");
    for alias in ["fable", "opus", "sonnet", "haiku"] {
        assert_eq!(
            cfg.config.profiles.get(alias).expect("build wizard config fixed tier profiles: the fixture collection must contain the expected entry").provider,
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
    let cfg = super::build_wizard_config(&state);
    assert_eq!(cfg.config.active_alias, "opus", "手动配置默认档位仍为 opus");
    assert_eq!(cfg.config.profiles.opus.provider, "anthropic");
    // 无固定档位的 provider：其余档位保持默认
    assert!(cfg.config.profiles.fable.is_default());
    assert!(cfg.config.profiles.sonnet.is_default());
    assert!(cfg.config.profiles.haiku.is_default());
}

/// [回归测试] 固定档位跟随 provider 自带的预设标志，不做 provider_id 反查。
#[test]
fn test_build_wizard_config_fixed_tiers_follow_provider_not_id() {
    // 手动配置即使复用网关 id 与 Key，也不套固定档位
    let mut manual = MigratedProvider::new(ProviderType::OpenAiCompatible);
    manual.provider_id = "peri".into();
    manual.api_key = "public".into();
    let cfg = super::build_wizard_config(&SetupWizardState {
        providers: vec![manual],
        ..Default::default()
    });
    assert_eq!(cfg.config.active_alias, "opus");

    // 预设 provider 改 id 后仍保留固定档位，并绑定到新 id
    let mut from_preset = PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1].to_migrated();
    from_preset.provider_id = "my-gateway".into();
    let cfg = super::build_wizard_config(&SetupWizardState {
        providers: vec![from_preset],
        ..Default::default()
    });
    assert_eq!(cfg.config.active_alias, "sonnet");
    for alias in ["fable", "opus", "sonnet", "haiku"] {
        assert_eq!(
            cfg.config.profiles.get(alias).expect("build wizard config fixed tiers follow provider not id: the fixture collection must contain the expected entry").provider,
            "my-gateway",
            "{alias} 档位应绑定新 id"
        );
    }
}

/// [回归测试] setup 检测必须与实际 provider 构造使用相同的 active_alias。
#[test]
#[serial]
fn test_needs_setup_empty_alias_matches_runtime_resolution() {
    isolated(
        "test_needs_setup_empty_alias_matches_runtime_resolution",
        || {
            let mut cfg = super::build_wizard_config(&SetupWizardState {
                providers: vec![PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1].to_migrated()],
                ..Default::default()
            });
            cfg.config.active_alias.clear();
            assert!(crate::app::agent::LlmProvider::from_config(&cfg).is_none());
            assert!(super::needs_setup(&cfg.config));
        },
    );
}

/// [回归测试] 全局 key + 工作区配置下，在 /setup 修改 provider endpoint：
/// workspace 保存会剥离继承凭据，accepted snapshot 不再有可用 provider。
/// 必须在落盘前拒绝，旧配置保持不变；重载后 provider 不能丢失。
#[test]
fn test_setup_endpoint_change_without_own_key_keeps_persisted_provider() {
    isolated(
        "test_setup_endpoint_change_without_own_key_keeps_persisted_provider",
        || {
            let root = std::env::current_dir().expect("setup endpoint change without own key keeps persisted provider: isolated fixture workspace must remain accessible");
            let home = PathBuf::from(std::env::var_os("HOME").expect("setup endpoint change without own key keeps persisted provider: the isolated fixture environment must contain its configured value"));
            let cwd = root.join("workspace");
            std::fs::create_dir_all(cwd.join(".peri")).expect("setup endpoint change without own key keeps persisted provider: isolated fixture workspace must remain accessible");
            let global = home.join(".peri/settings.json");
            let cfg: crate::config::PeriConfig = serde_json::from_value(serde_json::json!({
                "config": {
                    "active_alias": "opus",
                    "language": "zh-CN",
                    "profiles": {
                        "opus": {
                            "provider": "openai",
                            "model": "session-model",
                            "effort": "high"
                        }
                    },
                    "providers": [{
                        "id": "openai",
                        "name": "Global OpenAI",
                        "type": "openai",
                        "apiKey": "global-secret",
                        "baseUrl": "https://old.example/v1",
                        "models": {
                            "fable": "fable-model",
                            "opus": "opus-model",
                            "sonnet": "sonnet-model",
                            "haiku": "haiku-model"
                        }
                    }]
                }
            }))
            .expect("setup endpoint change without own key keeps persisted provider: fixture JSON must match the expected schema");
            crate::config::save_to(&cfg, &global).expect("setup endpoint change without own key keeps persisted provider: the save_to fixture operation uses valid inputs");
            let global_before = std::fs::read(&global).expect("setup endpoint change without own key keeps persisted provider: isolated fixture workspace must remain accessible");
            let workspace = cwd.join(".peri/settings.json");
            let workspace_before = r#"{"config":{"language":"zh-CN"}}"#;
            std::fs::write(&workspace, workspace_before).expect("setup endpoint change without own key keeps persisted provider: isolated fixture workspace must remain accessible");

            let source = std::sync::Arc::new(
                crate::config::ConfigSource::load_at(&cwd, global.clone()).expect("setup endpoint change without own key keeps persisted provider: the load_at fixture operation uses valid inputs"),
            );
            assert!(
                crate::kit::atoms::CONFIG_SOURCE_HANDLE
                    .set(source.clone())
                    .is_ok()
            );

            let mut state = super::state_from_config(&source.loaded_merged());
            assert_eq!(
                state.providers[0].api_key, "global-secret",
                "加载后的候选应包含从 global 继承的凭据"
            );
            state.providers[0].base_url = "https://changed.example/v1".into();

            let error = super::save_setup(&state)
                .expect_err("accepted snapshot 无可用 provider 时必须拒绝保存");
            assert!(
                error.to_string().contains("no usable active provider"),
                "错误必须说明 accepted 配置没有可用 provider: {error}"
            );

            assert_eq!(
                std::fs::read(&workspace).expect("setup endpoint change without own key keeps persisted provider: the fixture storage and layout must support the requested operation"),
                workspace_before.as_bytes(),
                "拒绝保存时不得写工作区文件"
            );
            assert_eq!(std::fs::read(&global).expect("setup endpoint change without own key keeps persisted provider: the fixture storage and layout must support the requested operation"), global_before);
            assert!(
                !std::fs::read_to_string(&workspace)
                    .expect("setup endpoint change without own key keeps persisted provider: the fixture storage and layout must support the requested operation")
                    .contains("global-secret")
            );

            let reloaded = crate::config::ConfigSource::load_at(&cwd, global).expect("setup endpoint change without own key keeps persisted provider: the load_at fixture operation uses valid inputs");
            let merged = reloaded.loaded_merged();
            let provider = merged
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "openai")
                .expect("拒绝保存后 provider 必须仍在");
            assert_eq!(provider.base_url, "https://old.example/v1");
            assert_eq!(provider.api_key, "global-secret");
            assert!(
                crate::app::agent::LlmProvider::from_config(&merged).is_some(),
                "重载后 provider 不能丢失"
            );
        },
    );
}

#[test]
fn test_wizard_replaces_broken_active_selection_with_usable_profile() {
    // 普通 provider：修复为默认档位绑定 opus
    let mut cfg = crate::config::PeriConfig::default();
    cfg.config.active_alias = "missing".into();
    let mut plain = MigratedProvider::new(ProviderType::Anthropic);
    plain.api_key = "sk-test".into();
    let draft = SetupWizardState {
        from_command: true,
        providers: vec![plain],
        ..Default::default()
    };
    let merged = super::merge_setup(&draft, cfg);
    assert!(crate::app::agent::LlmProvider::from_config(&merged).is_some());
    assert_eq!(merged.config.active_alias, "opus");

    // 免密钥网关预设：采用其固定档位配置的 active_alias
    let mut cfg = crate::config::PeriConfig::default();
    cfg.config.active_alias = "missing".into();
    let draft = SetupWizardState {
        from_command: true,
        providers: vec![PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1].to_migrated()],
        ..Default::default()
    };
    let merged = super::merge_setup(&draft, cfg);
    assert!(crate::app::agent::LlmProvider::from_config(&merged).is_some());
    assert_eq!(merged.config.active_alias, "sonnet");
}

/// [回归测试] 已配置用户重开表单时，仍可解析的档位选择不得被重置或覆盖。
#[test]
fn test_wizard_preserves_resolvable_selection_when_reopened() {
    let mut provider = MigratedProvider::new(ProviderType::Anthropic);
    provider.api_key = "sk-test".into();
    let seed = SetupWizardState {
        from_command: true,
        providers: vec![provider],
        ..Default::default()
    };
    let mut cfg = super::build_wizard_config(&seed);
    cfg.config.active_alias = "sonnet".into();
    cfg.config
        .profiles
        .get_mut("sonnet")
        .expect("sonnet profile")
        .effort = "high".into();
    cfg.config
        .profiles
        .get_mut("opus")
        .expect("opus profile")
        .effort = "max".into();

    let reopened = super::state_from_config(&cfg);
    let merged = super::merge_setup(&reopened, cfg);

    assert_eq!(
        merged.config.active_alias, "sonnet",
        "valid active selection must survive provider edits"
    );
    assert_eq!(
        merged
            .config
            .profiles
            .get("sonnet")
            .expect("sonnet profile")
            .effort,
        "high"
    );
    assert_eq!(
        merged
            .config
            .profiles
            .get("opus")
            .expect("opus profile")
            .effort,
        "max"
    );
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("connectivity http status and errors do not expose url or body: fixture task and request must complete successfully");
        let address = listener.local_addr().expect("connectivity http status and errors do not expose url or body: the fixture listener must expose its bound local address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("connectivity http status and errors do not expose url or body: fixture task and request must complete successfully");
            let mut request = [0; 4096];
            let received = stream.read(&mut request).await.expect("connectivity http status and errors do not expose url or body: fixture task and request must complete successfully");
            assert!(received > 0, "客户端应发送 HTTP 请求");
            stream.write_all(response.as_bytes()).await.expect("connectivity http status and errors do not expose url or body: fixture task and request must complete successfully");
        });
        let result =
            super::test_connectivity(&format!("http://{address}/v1?credential=private-query"))
                .await;
        server.await.expect("connectivity http status and errors do not expose url or body: fixture task and request must complete successfully");
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
        let (success, message) = super::test_connectivity(url).await;
        assert!(!success);
        assert!(!message.contains("private-"));
        assert!(!message.contains(url));
    }
}

/// [回归测试] 超时覆盖“TCP 已连接但 HTTP 永不响应”，避免后台请求永久存活。
#[tokio::test]
async fn test_connectivity_stalled_http_response_times_out() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("connectivity stalled http response times out: fixture task and request must complete successfully");
    let address = listener.local_addr().expect("connectivity stalled http response times out: the fixture listener must expose its bound local address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("connectivity stalled http response times out: fixture task and request must complete successfully");
        let mut request = [0; 4096];
        let received = stream.read(&mut request).await.expect("connectivity stalled http response times out: fixture task and request must complete successfully");
        assert!(received > 0, "客户端应发送 HTTP 请求");
        std::future::pending::<()>().await;
        drop(stream);
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(7),
        super::test_connectivity(&format!("http://{address}")),
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
    let cfg = super::build_wizard_config(&SetupWizardState {
        providers: vec![provider],
        ..Default::default()
    });
    let mut draft = super::state_from_config(&cfg);
    let provider = &mut draft.providers[0];
    provider.provider_type.cycle();
    provider.refresh_provider_defaults();
    assert_eq!(provider.provider_id, "anthropic");
    assert_eq!(
        provider.base_url,
        ProviderType::OpenAiCompatible.default_base_url()
    );
    let merged = super::merge_setup(&draft, cfg);
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
    let cfg = super::build_wizard_config(&SetupWizardState {
        providers: vec![PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1].to_migrated()],
        ..Default::default()
    });
    let mut draft = super::state_from_config(&cfg);
    draft.form_focus = FormField::ProviderId;
    assert!(!draft.active_field_is_editable());
    draft.set_active_field_value("other".into());
    assert_eq!(draft.providers[0].provider_id, "peri");
    draft.providers[0].provider_id = "other".into();
    assert!(
        super::save_setup(&draft).is_err(),
        "非法改名应在任何磁盘读写前拒绝"
    );
}

#[test]
fn test_provider_identity_provenance_survives_state_roundtrip() {
    let cfg = super::build_wizard_config(&SetupWizardState {
        providers: vec![PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1].to_migrated()],
        ..Default::default()
    });
    let state = super::state_from_config(&cfg);
    let encoded = serde_json::to_value(state).expect(
        "provider identity provenance survives state roundtrip: fixture must serialize to JSON",
    );
    let restored: SetupWizardState = serde_json::from_value(encoded).expect("provider identity provenance survives state roundtrip: fixture JSON must match the expected schema");
    assert!(!restored.providers[0].provider_id_is_editable());
    let mut old_provider =
        serde_json::to_value(PROVIDER_PRESETS[PROVIDER_PRESETS.len() - 1].to_migrated()).expect(
            "provider identity provenance survives state roundtrip: fixture must serialize to JSON",
        );
    old_provider
        .as_object_mut()
        .expect("provider identity provenance survives state roundtrip: fixture must have the expected variant")
        .remove("original_provider_id");
    let restored: MigratedProvider = serde_json::from_value(old_provider).expect("provider identity provenance survives state roundtrip: fixture JSON must match the expected schema");
    assert!(restored.provider_id_is_editable());
}

/// 独立子进程内的全局 key + 工作区 fixture。
fn write_inherited_key_fixture() -> (PathBuf, PathBuf, PathBuf, Vec<u8>) {
    let root = std::env::current_dir()
        .expect("write inherited key fixture: isolated fixture workspace must remain accessible");
    let home = PathBuf::from(std::env::var_os("HOME").expect("write inherited key fixture: the isolated fixture environment must contain its configured value"));
    let cwd = root.join("workspace");
    std::fs::create_dir_all(cwd.join(".peri"))
        .expect("write inherited key fixture: isolated fixture workspace must remain accessible");
    let global = home.join(".peri/settings.json");
    let cfg: crate::config::PeriConfig = serde_json::from_value(serde_json::json!({
        "config": {
            "active_alias": "opus",
            "language": "zh-CN",
            "profiles": {
                "opus": {
                    "provider": "openai",
                    "model": "session-model",
                    "effort": "high"
                }
            },
            "providers": [{
                "id": "openai",
                "name": "Global OpenAI",
                "type": "openai",
                "apiKey": "global-secret",
                "baseUrl": "https://old.example/v1",
                "models": {
                    "fable": "fable-model",
                    "opus": "opus-model",
                    "sonnet": "sonnet-model",
                    "haiku": "haiku-model"
                }
            }]
        }
    }))
    .expect("write inherited key fixture: fixture JSON must match the expected schema");
    crate::config::save_to(&cfg, &global)
        .expect("write inherited key fixture: the save_to fixture operation uses valid inputs");
    let global_before = std::fs::read(&global)
        .expect("write inherited key fixture: isolated fixture workspace must remain accessible");
    let workspace = cwd.join(".peri/settings.json");
    std::fs::write(&workspace, r#"{"config":{"language":"zh-CN"}}"#)
        .expect("write inherited key fixture: isolated fixture workspace must remain accessible");
    (cwd, global, workspace, global_before)
}

#[path = "setup_wizard_test/configuration_persistence.rs"]
mod configuration_persistence;

fn isolated(name: &str, scenario: fn()) {
    let module = module_path!()
        .split_once("::")
        .expect("the test module has a crate prefix")
        .1;
    run_isolated(module, name, scenario);
}

use super::{build_wizard_config, merge_setup, save_setup, state_from_config};
