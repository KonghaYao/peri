//! Login 配置闭环的真实提交测试：分层配置文件 + ACP MPSC frames。
//!
//! 每个场景独立子进程：`PERI_CONFIG_HANDLE` / `CONFIG_SOURCE_HANDLE` 是进程级
//! OnceLock，同进程无法为每个场景重置。模式与 `model/commit_test.rs` 一致。

use super::super::LoginPanelMode;
use super::super::auth::AuthController;

use super::super::edit_handler::LoginEditContext;
use super::super::edit_state::{LoginEditState, LoginErrorKind, LoginField, ProviderKind};

use crate::acp_client::{AcpNotification, AcpTuiClient};
use crate::config::{ConfigSource, PeriConfig, Profiles, ProviderModels};

use peri_acp::transport::AcpTransport;
use peri_acp::transport::mpsc::MpscServerTransport;
use peri_acp::transport::types::AcpError;
use peri_acp::transport::types::IncomingMessage;
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use std::future::Future;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

const CHILD_CASE: &str = "PERI_LOGIN_COMMIT_CASE";

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn isolated(name: &str, scenario: impl Future<Output = ()>) {
    run_isolated(
        name,
        module_path!()
            .split_once("::")
            .expect("isolated: the fixture text must contain its expected delimiter")
            .1,
        scenario,
    );
}

/// 子模块（`commit::auth_save`）必须传自己的 `module_path!()`：宏在定义处展开，
/// 不能在共享 runner 里推导调用方模块。
pub(super) fn run_isolated(name: &str, test_module: &str, scenario: impl Future<Output = ()>) {
    if std::env::var(CHILD_CASE).as_deref() == Ok(name) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("run isolated: isolated fixture workspace must remain accessible")
            .block_on(scenario);
        return;
    }
    let dir = tempfile::tempdir()
        .expect("run isolated: isolated fixture workspace must remain accessible");
    let log_path = dir.path().join("child.log");
    let home = dir.path().join("home");
    let cwd = dir.path().join("cwd");
    std::fs::create_dir_all(&home)
        .expect("run isolated: isolated fixture workspace must remain accessible");
    std::fs::create_dir_all(&cwd)
        .expect("run isolated: isolated fixture workspace must remain accessible");
    let log = std::fs::File::create(&log_path).expect(
        "run isolated: the fixture storage and layout must support the requested operation",
    );
    let child = Command::new(
        std::env::current_exe()
            .expect("run isolated: isolated fixture workspace must remain accessible"),
    )
    .args(["--exact", &format!("{test_module}::{name}"), "--nocapture"])
    .env(CHILD_CASE, name)
    .env("HOME", &home)
    .env("USERPROFILE", &home)
    .current_dir(&cwd)
    .stdout(
        log.try_clone()
            .expect("run isolated: the try_clone fixture operation uses valid inputs"),
    )
    .stderr(log)
    .spawn()
    .expect("run isolated: isolated fixture workspace must remain accessible");
    let mut guard = ChildGuard(child);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = guard
            .0
            .try_wait()
            .expect("run isolated: the try_wait fixture operation uses valid inputs")
        {
            let output = std::fs::read_to_string(&log_path)
                .expect("run isolated: isolated fixture workspace must remain accessible");
            assert!(status.success(), "{name}: {status}\n{output}");
            assert!(
                output.contains("1 passed"),
                "child filter missed test: {output}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "{name}: child watchdog expired");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn initial_config() -> PeriConfig {
    serde_json::from_value(json!({"config": {
        "active_alias": "sonnet",
        "providers": [
            {"id":"base", "type":"anthropic", "apiKey":"base-key",
             "models":{"fable":"base-fable", "opus":"base-opus", "sonnet":"base-sonnet", "haiku":"base-haiku"}},
            {"id":"plan", "type":"openai_responses", "apiKey":"",
             "auth":{"kind":"chatgpt_plan", "store":std::env::temp_dir().join("peri-login-commit"), "account":"work",
                     "subject":"subject", "clientId":"client"},
             "models":{"fable":"plan-fable", "opus":"plan-opus", "sonnet":"plan-sonnet", "haiku":"plan-haiku"}}
        ],
        "profiles": {
            "fable":{"provider":"base", "contextWindow":1000},
            "opus":{"provider":"base", "contextWindow":2000},
            "sonnet":{"provider":"base", "contextWindow":3000},
            "haiku":{"provider":"plan", "contextWindow":4000, "effort":"max"}
        }
    }}))
    .expect("initial config: fixture JSON must match the expected schema")
}

struct Fixture {
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    global: PathBuf,
    target: PathBuf,
    original_global: Vec<u8>,
    source: Arc<ConfigSource>,
    config: Arc<parking_lot::RwLock<PeriConfig>>,
    client: Arc<AcpTuiClient>,
    server: MpscServerTransport,
    notifications: tokio::sync::mpsc::Receiver<AcpNotification>,
}

impl Fixture {
    fn new(workspace: bool) -> Self {
        crate::kit::atoms::init_atoms();
        crate::i18n::init(Some("en"));
        let dir =
            tempfile::tempdir().expect("new: isolated fixture workspace must remain accessible");
        let cwd = dir.path().join("workspace");
        std::fs::create_dir_all(&cwd)
            .expect("new: isolated fixture workspace must remain accessible");
        let global = dir.path().join("global.json");
        let initial = initial_config();
        crate::config::save_to(&initial, &global)
            .expect("new: the save_to fixture operation uses valid inputs");
        let target = if workspace {
            let target = cwd.join(".peri/settings.json");
            crate::config::save_to(&PeriConfig::default(), &target)
                .expect("new: the save_to fixture operation uses valid inputs");
            target
        } else {
            global.clone()
        };
        let original_global =
            std::fs::read(&global).expect("new: isolated fixture workspace must remain accessible");
        let source = Arc::new(
            ConfigSource::load_at(&cwd, global.clone())
                .expect("new: the load_at fixture operation uses valid inputs"),
        );
        let mut loaded = source.loaded_merged();
        // Workspace defaults make serde apply `active_alias = opus`; keep the fixture's
        // effective alias stable across global/workspace cases.
        loaded.config.active_alias = "sonnet".into();
        let config = Arc::new(parking_lot::RwLock::new(loaded));
        assert!(PERI_CONFIG_HANDLE.set(config.clone()).is_ok());
        assert!(CONFIG_SOURCE_HANDLE.set(source.clone()).is_ok());
        let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
        let (client, tx, notifications) = AcpTuiClient::new(transport);
        client.spawn_pump(tx);
        let client = Arc::new(client);
        assert!(ACP_CLIENT_HANDLE.set(client.clone()).is_ok());
        Self {
            _dir: dir,
            cwd,
            global,
            target,
            original_global,
            source,
            config,
            client,
            server,
            notifications,
        }
    }

    fn revision(&self) -> crate::config::ConfigurationRevision {
        self.source
            .snapshot()
            .expect("revision: the snapshot fixture operation uses valid inputs")
            .revision()
    }

    async fn recv(&self) -> IncomingMessage {
        tokio::time::timeout(Duration::from_secs(3), self.server.recv())
            .await
            .expect("login save must reach real transport")
            .expect("recv: fixture task and request must complete successfully")
    }

    fn assert_disk_and_frame(&self, params: &Value) {
        assert!(
            self.config.try_write().is_some(),
            "config lock must be released before RPC"
        );
        let expected = self.config.read().clone();
        let wire: PeriConfig = serde_json::from_value(params["config"].clone())
            .expect("assert disk and frame: fixture JSON must match the expected schema");
        assert_eq!(wire, expected);
        let loaded = ConfigSource::load_at(&self.cwd, self.global.clone())
            .expect("assert disk and frame: the load_at fixture operation uses valid inputs");
        assert_eq!(
            loaded.loaded_merged(),
            expected,
            "save must precede RPC frame"
        );
    }

    async fn expect_request(&self, fail_rpc: bool) {
        let IncomingMessage::Request { id, method, params } = self.recv().await else {
            panic!("expected update request")
        };
        assert_eq!(method, "session/update_config");
        assert!(
            params.get("sessionId").is_none(),
            "host configuration must not target the active session"
        );
        self.assert_disk_and_frame(&params);
        self.server
            .send_response(
                id,
                if fail_rpc {
                    Err(AcpError::new(-32603, "fixture rejects update"))
                } else {
                    Ok(json!({}))
                },
            )
            .await
            .expect("expect request: fixture task and request must complete successfully");
    }

    async fn finish(mut self) {
        self.client.close();
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.notifications.recv().await.is_some() {}
        })
        .await
        .expect("notification pump must stop after close");
        assert!(
            tokio::time::timeout(Duration::from_secs(3), self.server.recv())
                .await
                .expect("finish: the fixture channel must exchange its expected message")
                .is_none()
        );
    }
}

fn new_responses_state(fixture: &Fixture) -> LoginEditState {
    let cfg = fixture.config.read().clone();
    let mut state = LoginEditState::new_provider(&cfg, Some(fixture.revision()));
    state.kind = Some(ProviderKind::OpenAiResponses);
    state.raw_type = ProviderKind::OpenAiResponses.as_str().to_string();
    state.provider_id = "new-responses".into();
    state.api_key = "new-key".into();
    state.base_url = String::new();
    state.models = ProviderModels {
        fable: "new-fable".into(),
        opus: "new-opus".into(),
        sonnet: "new-sonnet".into(),
        haiku: "new-haiku".into(),
    };
    state
}

#[test]
fn login_commit_api_key_provider_writes_global_disk_and_frame() {
    isolated(
        "login_commit_api_key_provider_writes_global_disk_and_frame",
        async {
            let fixture = Fixture::new(false);
            let mut state = new_responses_state(&fixture);
            assert!(super::super::config_store::save_login_edit(&mut state));

            assert_eq!(
                PROVIDER_LIST
                    .state()
                    .read()
                    .iter()
                    .find(|provider| provider.id == "new-responses")
                    .map(|provider| provider.provider_type.as_str()),
                Some("openai_responses")
            );
            fixture.expect_request(false).await;
            let saved = fixture.config.read().clone();
            assert_eq!(saved.config.active_alias, "sonnet");
            let provider = saved
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "new-responses")
                .expect("login commit api key provider writes global disk and frame: fixture must contain the searched item");
            assert_eq!(provider.api_key, "new-key");
            assert_eq!(
                provider.auth,
                Some(peri_acp::provider::config::ProviderAuthConfig::ApiKey)
            );
            let sonnet = saved.config.profiles.sonnet.clone();
            assert_eq!(
                sonnet,
                initial_config().config.profiles.sonnet,
                "Login must not bind or rewrite any profile"
            );
            for alias in ["fable", "opus", "haiku"] {
                assert_eq!(
                    saved.config.profiles.get(alias),
                    initial_config().config.profiles.get(alias),
                    "{alias} must stay unchanged"
                );
            }
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_api_key_provider_writes_workspace_and_preserves_global_bytes() {
    isolated(
        "login_commit_api_key_provider_writes_workspace_and_preserves_global_bytes",
        async {
            let fixture = Fixture::new(true);
            let mut state = new_responses_state(&fixture);
            let saved = super::super::config_store::save_login_edit(&mut state);
            assert!(
                saved,
                "workspace save failed: inline={:?} notice={:?}",
                state
                    .inline_error
                    .as_ref()
                    .map(|error| error.message.clone()),
                NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|note| note.message.clone())
            );
            fixture.expect_request(false).await;
            assert_eq!(
                std::fs::read(&fixture.global).expect("login commit api key provider writes workspace and preserves global bytes: the fixture storage and layout must support the requested operation"),
                fixture.original_global,
                "workspace save must not rewrite the global file"
            );
            assert!(fixture.target.exists());
            let loaded = ConfigSource::load_at(&fixture.cwd, fixture.global.clone()).expect("login commit api key provider writes workspace and preserves global bytes: the load_at fixture operation uses valid inputs");
            assert_eq!(loaded.loaded_merged(), fixture.config.read().clone());
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_plan_reference_is_preserved_and_has_empty_api_key() {
    isolated(
        "login_commit_plan_reference_is_preserved_and_has_empty_api_key",
        async {
            let fixture = Fixture::new(false);
            let cfg = fixture.config.read().clone();
            let mut state =
                LoginEditState::edit_provider(&cfg, "plan", Some(fixture.revision())).expect("login commit plan reference is preserved and has empty api key: the edit_provider fixture operation uses valid inputs");
            assert!(super::super::config_store::save_login_edit(&mut state));
            fixture.expect_request(false).await;

            let provider = fixture
                .config
                .read()
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "plan")
                .cloned()
                .expect("login commit plan reference is preserved and has empty api key: fixture must contain the searched item");
            assert!(matches!(
                provider.auth,
                Some(peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan { .. })
            ));
            assert!(provider.api_key.is_empty());
            let encoded = serde_json::to_value(&provider).expect("login commit plan reference is preserved and has empty api key: fixture must serialize to JSON");
            assert_eq!(encoded["apiKey"], "");
            assert!(encoded.get("accessToken").is_none());
            assert!(encoded.get("refreshToken").is_none());
            // 隐藏字段仍完整落盘：渲染不展示，但草稿与磁盘 JSON 不丢值。
            assert_eq!(
                encoded["auth"]["store"],
                json!(std::env::temp_dir().join("peri-login-commit"))
            );
            assert_eq!(encoded["auth"]["account"], "work");
            assert_eq!(encoded["auth"]["subject"], "subject");
            assert_eq!(encoded["auth"]["clientId"], "client");
            let disk = ConfigSource::load_at(&fixture.cwd, fixture.global.clone()).expect("login commit plan reference is preserved and has empty api key: the load_at fixture operation uses valid inputs");
            let disk_provider = disk
                .loaded_merged()
                .config
                .providers
                .into_iter()
                .find(|provider| provider.id == "plan")
                .expect("saved provider must be on disk");
            let disk_encoded = serde_json::to_value(&disk_provider).expect("login commit plan reference is preserved and has empty api key: fixture must serialize to JSON");
            assert_eq!(disk_encoded["auth"]["account"], "work");
            assert_eq!(disk_encoded["auth"]["subject"], "subject");
            assert_eq!(disk_encoded["auth"]["clientId"], "client");
            assert_eq!(
                fixture.config.read().config.profiles,
                initial_config().config.profiles,
                "editing a provider must not rewrite existing profile parameters"
            );
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_save_failure_keeps_memory_disk_and_pushes_nothing() {
    isolated(
        "login_commit_save_failure_keeps_memory_disk_and_pushes_nothing",
        async {
            let fixture = Fixture::new(false);
            let mut state = new_responses_state(&fixture);
            let before = fixture.config.read().clone();
            std::fs::remove_file(&fixture.target).expect("login commit save failure keeps memory disk and pushes nothing: the fixture storage and layout must support the requested operation");
            std::fs::create_dir(&fixture.target).expect("login commit save failure keeps memory disk and pushes nothing: isolated fixture workspace must remain accessible");

            assert!(!super::super::config_store::save_login_edit(&mut state));
            assert_eq!(*fixture.config.read(), before);
            assert!(
                state.inline_error.is_none(),
                "I/O failure is reported by the shared notification, draft stays"
            );
            let notice = NOTIFICATION
                .state()
                .read()
                .as_ref()
                .map(|note| note.message.clone())
                .unwrap_or_default();
            assert!(
                notice.contains("Configuration save failed"),
                "must reuse config-save-failed: {notice}"
            );
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_revision_conflict_does_not_overwrite_external_write() {
    isolated(
        "login_commit_revision_conflict_does_not_overwrite_external_write",
        async {
            let fixture = Fixture::new(false);
            let mut state = new_responses_state(&fixture);
            let before = fixture.config.read().clone();

            // 外部写入（例如另一个面板/进程）推进 revision。
            let mut external = before.clone();
            external.config.active_alias = "haiku".into();
            fixture
                .source
                .save(fixture.revision(), &external)
                .expect("external save");

            assert!(!super::super::config_store::save_login_edit(&mut state));
            let error = state
                .inline_error
                .as_ref()
                .expect("conflict must be inline");
            assert_eq!(error.message, crate::i18n::tr("config-draft-conflict"));
            assert_eq!(
                *fixture.config.read(),
                before,
                "冲突时不得发布旧草稿或外部配置"
            );
            let disk = ConfigSource::load_at(&fixture.cwd, fixture.global.clone())
                .expect("login commit revision conflict does not overwrite external write: the load_at fixture operation uses valid inputs")
                .loaded_merged();
            assert_eq!(disk.config.active_alias, "haiku");
            assert!(
                disk.config
                    .providers
                    .iter()
                    .all(|provider| provider.id != "new-responses"),
                "旧草稿不得覆盖外部写入"
            );
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_acp_failure_reports_sync_warning_after_saved() {
    isolated(
        "login_commit_acp_failure_reports_sync_warning_after_saved",
        async {
            let fixture = Fixture::new(false);
            let mut state = new_responses_state(&fixture);
            assert!(super::super::config_store::save_login_edit(&mut state));
            fixture.expect_request(true).await;

            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let message = NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|note| note.message.clone());
                if let Some(message) = message
                    && message.contains("runtime sync failed")
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "ACP sync failure must be visible in NOTIFICATION"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let saved = fixture.config.read().clone();
            assert!(
                saved
                    .config
                    .providers
                    .iter()
                    .any(|provider| provider.id == "new-responses"),
                "ACP failure must not roll back the saved settings"
            );
            let disk = ConfigSource::load_at(&fixture.cwd, fixture.global.clone())
                .expect("login commit acp failure reports sync warning after saved: the load_at fixture operation uses valid inputs")
                .loaded_merged();
            assert_eq!(disk, saved);
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_browse_label_keeps_unknown_type_raw() {
    assert_eq!(
        super::super::render::provider_type_display("openai_responses"),
        "OpenAI Responses"
    );
    assert_eq!(
        super::super::render::provider_type_display("future_type"),
        "future_type"
    );
}

fn ctrl_s() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
}

#[test]
fn login_commit_rename_preserves_profile_parameters() {
    isolated("login_commit_rename_preserves_profile_parameters", async {
        let fixture = Fixture::new(false);
        let cfg = fixture.config.read().clone();
        let mut state =
            LoginEditState::edit_provider(&cfg, "base", Some(fixture.revision())).expect("login commit rename preserves profile parameters: the edit_provider fixture operation uses valid inputs");
        state.provider_id = "base-renamed".into();
        assert!(super::super::config_store::save_login_edit(&mut state));
        fixture.expect_request(false).await;

        let saved = fixture.config.read().clone();
        assert!(
            saved
                .config
                .providers
                .iter()
                .any(|provider| provider.id == "base-renamed"),
            "renamed provider must be persisted"
        );
        let baseline = initial_config();
        for alias in Profiles::ALL {
            let mut expected = baseline.config.profiles.get(alias).expect("login commit rename preserves profile parameters: fixture must contain the requested entry").clone();
            if expected.provider == "base" {
                expected.provider = "base-renamed".to_string();
            }
            assert_eq!(
                saved.config.profiles.get(alias),
                Some(&expected),
                "{alias} must keep model/window/effort; only its reference is renamed"
            );
        }
        let disk = ConfigSource::load_at(&fixture.cwd, fixture.global.clone()).expect("login commit rename preserves profile parameters: the load_at fixture operation uses valid inputs");
        assert_eq!(disk.loaded_merged(), saved);
        fixture.finish().await;
    });
}

#[test]
fn login_commit_plan_reference_saves_workspace_without_rewriting_global() {
    isolated(
        "login_commit_plan_reference_saves_workspace_without_rewriting_global",
        async {
            let fixture = Fixture::new(true);
            let cfg = fixture.config.read().clone();
            let mut state =
                LoginEditState::edit_provider(&cfg, "plan", Some(fixture.revision())).expect("login commit plan reference saves workspace without rewriting global: the edit_provider fixture operation uses valid inputs");
            assert!(super::super::config_store::save_login_edit(&mut state));
            fixture.expect_request(false).await;
            assert_eq!(
                std::fs::read(&fixture.global).expect("login commit plan reference saves workspace without rewriting global: the fixture storage and layout must support the requested operation"),
                fixture.original_global
            );
            let loaded = ConfigSource::load_at(&fixture.cwd, fixture.global.clone()).expect("login commit plan reference saves workspace without rewriting global: the load_at fixture operation uses valid inputs");
            assert_eq!(loaded.loaded_merged(), fixture.config.read().clone());
            let provider = fixture
                .config
                .read()
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "plan")
                .cloned()
                .expect("login commit plan reference saves workspace without rewriting global: fixture must contain the searched item");
            assert!(matches!(
                provider.auth,
                Some(peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan { .. })
            ));
            fixture.finish().await;
        },
    );
}

/// P0-85：字段错误与 revision 冲突必须分类；冲突后 Ctrl+R 从权威配置重新基线化，
/// 用新 revision 保存成功，且旧草稿不得覆盖外部写入。
#[test]
fn login_commit_conflict_reload_rebases_and_preserves_external_write() {
    isolated(
        "login_commit_conflict_reload_rebases_and_preserves_external_write",
        async {
            let fixture = Fixture::new(false);
            let cfg = fixture.config.read().clone();
            let mut state =
                LoginEditState::edit_provider(&cfg, "base", Some(fixture.revision())).expect("login commit conflict reload rebases and preserves external write: the edit_provider fixture operation uses valid inputs");
            let stale_revision = state.expected_revision;
            state.api_key = "stale-key".into();

            // 1) 普通字段校验错误：不得被归类为 revision 冲突。
            let provider_id = state.provider_id.clone();
            state.provider_id.clear();
            assert!(!super::super::config_store::save_login_edit(&mut state));
            let field_error = state
                .inline_error
                .as_ref()
                .map(|error| (error.kind, error.message.clone()));
            assert_eq!(
                field_error,
                Some((
                    LoginErrorKind::Field,
                    crate::i18n::tr("login-error-id-empty")
                )),
                "字段校验错误必须与冲突区分"
            );
            state.provider_id = provider_id;
            state.inline_error = None;

            // 2) 外部写入推进 revision；旧草稿保存必须失败且不覆盖外部值。
            let mut external = fixture.config.read().clone();
            external.config.active_alias = "haiku".into();
            external.config.providers[0].api_key = "external-key".into();
            fixture
                .source
                .save(fixture.revision(), &external)
                .expect("external save");
            assert!(!super::super::config_store::save_login_edit(&mut state));
            let conflict = state
                .inline_error
                .as_ref()
                .map(|error| (error.kind, error.message.clone()));
            assert_eq!(
                conflict,
                Some((
                    LoginErrorKind::Conflict,
                    crate::i18n::tr("config-draft-conflict")
                )),
                "revision 冲突必须单独标记"
            );
            assert_eq!(state.expected_revision, stale_revision);
            assert_eq!(
                fixture.config.read().config.active_alias,
                "sonnet",
                "冲突时不得发布外部配置或旧草稿"
            );

            // 3) 按提示 Ctrl+R：真实按键路径重读权威配置并重建草稿。
            let mut mode = LoginPanelMode::Edit;
            let mut edit_state = Some(state);
            let mut focus = LoginField::Confirm;
            let mut cursor = 0;
            let mut auth = AuthController::new();
            let _ = super::super::edit_handler::handle_login_edit_keys(
                LoginEditContext {
                    mode: &mut mode,
                    edit_state: &mut edit_state,
                    edit_focus: &mut focus,
                    edit_cursor: &mut cursor,
                    auth: &mut auth,
                },
                false,
                None,
                &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            );
            assert_eq!(mode, LoginPanelMode::Edit);
            let mut recovered = edit_state.expect("重载后应重新基线化草稿");
            assert_eq!(
                recovered.expected_revision,
                Some(fixture.revision()),
                "重载后必须捕获最新 revision"
            );
            assert_ne!(recovered.expected_revision, stale_revision);
            assert!(recovered.inline_error.is_none());
            assert_eq!(
                fixture.config.read().config.active_alias,
                "haiku",
                "重载必须发布权威配置的最新值"
            );
            assert_eq!(
                recovered.api_key, "external-key",
                "重载后的草稿必须来自权威快照，而不是旧草稿"
            );

            // 4) 用新 revision 保存成功；外部值保持，旧草稿值不落盘。
            assert!(super::super::config_store::save_login_edit(&mut recovered));
            fixture.expect_request(false).await;
            let disk = ConfigSource::load_at(&fixture.cwd, fixture.global.clone())
                .expect("login commit conflict reload rebases and preserves external write: the load_at fixture operation uses valid inputs")
                .loaded_merged();
            assert_eq!(disk.config.active_alias, "haiku");
            assert_eq!(disk.config.providers[0].api_key, "external-key");
            assert!(
                disk.config
                    .providers
                    .iter()
                    .all(|provider| provider.api_key != "stale-key"),
                "旧草稿不得覆盖外部新值"
            );
            fixture.finish().await;
        },
    );
}

/// 设计 §4.3.2：编辑已有 provider 默认聚焦 Provider ID（类型仍锁定）。
#[test]
fn login_edit_existing_provider_defaults_focus_to_provider_id() {
    isolated(
        "login_edit_existing_provider_defaults_focus_to_provider_id",
        async {
            let fixture = Fixture::new(false);
            let mut edit_state = None;
            let mut focus = LoginField::ProviderType;
            let mut cursor = 0;
            assert!(super::super::edit_handler::enter_login_edit_mode(
                &mut edit_state,
                &mut focus,
                &mut cursor,
                "base"
            ));
            assert_eq!(focus, LoginField::ProviderId);
            assert_eq!(cursor, "base".chars().count());
            let state = edit_state.expect("已有 provider 必须进入编辑");
            assert_eq!(state.original_provider_id, "base");
            assert_eq!(state.kind, Some(ProviderKind::Anthropic));
            fixture.finish().await;
        },
    );
}

/// 鼠标点击 Confirm 与键盘 Enter 走同一保存入口：真实落盘、发布 accepted 并推送 ACP。
#[test]
fn login_commit_mouse_confirm_saves_provider() {
    isolated("login_commit_mouse_confirm_saves_provider", async {
        let fixture = Fixture::new(false);
        let state = new_responses_state(&fixture);
        let expected = state.build_candidate().expect("测试草稿必须可构造候选");
        let area = super::tall_area();
        let (tx, rx) = std::sync::mpsc::channel();

        let _buf = super::mount_panel_at(1, move |panel| {
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_state.write() = Some(state.clone());
            *panel.edit_focus.write() = LoginField::Confirm;
            *panel.edit_cursor.write() = 0;
            let row = super::edit_hit_row(
                &panel,
                super::super::render::EditHit::Field(LoginField::Confirm),
                area,
            );
            let result = panel.event(super::click_at(row, 3), area);
            tx.send((
                result,
                *panel.mode.read(),
                panel.edit_state.read().is_none(),
            ))
            .expect("测试结果通道仍打开");
        });

        let (result, mode, closed) = rx.try_recv().expect("点击 Confirm 必须走真实事件入口");
        assert_eq!(result, ratatui_kit::prelude::EventResult::Consumed);
        assert_eq!(mode, LoginPanelMode::Browse, "保存成功后回到 Browse");
        assert!(closed, "保存成功后释放草稿");

        let saved = fixture.config.read().clone();
        let provider = saved
            .config
            .providers
            .iter()
            .find(|provider| provider.id == "new-responses")
            .expect("鼠标 Confirm 必须落盘新 provider");
        let expected_provider = expected
            .config
            .providers
            .iter()
            .find(|provider| provider.id == "new-responses")
            .expect("候选必须包含新 provider");
        assert_eq!(provider, expected_provider, "键鼠必须产生同一配置结果");
        assert_eq!(
            saved.config.profiles,
            initial_config().config.profiles,
            "鼠标保存也必须保持所有既有档位绑定与参数"
        );

        fixture.expect_request(false).await;
        fixture.finish().await;
    });
}

#[path = "auth_save_commit_test.rs"]
mod auth_save;

use crate::kit::atoms::ACP_CLIENT_HANDLE;

use crate::kit::atoms::CONFIG_SOURCE_HANDLE;

use crate::kit::atoms::NOTIFICATION;

use crate::kit::atoms::PERI_CONFIG_HANDLE;

use crate::kit::atoms::PROVIDER_LIST;

#[path = "commit_test/paste_validation.rs"]
mod paste_validation;
