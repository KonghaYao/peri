//! Real model actions, layered configuration files and ACP MPSC frames.
//! Each scenario owns a process because the production handles are OnceLocks.
use super::super::window_edit::WindowEditState;
use super::*;
use crate::acp_client::{AcpNotification, AcpTuiClient};
use crate::config::{ConfigSource, PeriConfig, ProviderConfig, ProviderModels};
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, CONFIG_SOURCE_HANDLE, MODEL_HIGHLIGHT_UNTIL, NOTIFICATION, ServiceSnapshot,
};
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{AcpError, IncomingMessage},
};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

const CHILD_CASE: &str = "PERI_MODEL_COMMIT_CASE";

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn isolated(name: &str, scenario: impl Future<Output = ()>) {
    if std::env::var(CHILD_CASE).as_deref() == Ok(name) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(scenario);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("child.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let (_, test_module) = module_path!().split_once("::").unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("{test_module}::{name}"), "--nocapture"])
        .env(CHILD_CASE, name)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut guard = ChildGuard(child);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = guard.0.try_wait().unwrap() {
            let output = std::fs::read_to_string(&log_path).unwrap();
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

struct Fixture {
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    global: PathBuf,
    target: PathBuf,
    original_global: Vec<u8>,
    config: Arc<parking_lot::RwLock<PeriConfig>>,
    client: Arc<AcpTuiClient>,
    server: MpscServerTransport,
    notifications: tokio::sync::mpsc::Receiver<AcpNotification>,
}

impl Fixture {
    fn new(workspace: bool) -> Self {
        crate::kit::atoms::init_atoms();
        crate::i18n::init(Some("en"));
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let global = dir.path().join("global.json");
        let mut initial = PeriConfig::default();
        initial.config.active_alias = "sonnet".into();
        initial.config.providers = [("a", "openai"), ("b", "anthropic")]
            .into_iter()
            .map(|(id, provider_type)| ProviderConfig {
                id: id.into(),
                provider_type: provider_type.into(),
                models: ProviderModels {
                    opus: format!("{id}-opus"),
                    sonnet: format!("{id}-sonnet"),
                    haiku: format!("{id}-haiku"),
                    fable: format!("{id}-fable"),
                },
                ..Default::default()
            })
            .collect();
        initial.config.providers.push(ProviderConfig {
            id: "r".into(),
            provider_type: "openai_responses".into(),
            api_key: "r-key".into(),
            models: ProviderModels {
                opus: "r-opus".into(),
                sonnet: "r-sonnet".into(),
                haiku: "r-haiku".into(),
                fable: "r-fable".into(),
            },
            ..Default::default()
        });
        for alias in PROFILE_KEYS {
            initial.config.profiles.get_mut(alias).unwrap().provider = "a".into();
        }
        crate::config::save_to(&initial, &global).unwrap();
        let target = if workspace {
            let target = cwd.join(".peri/settings.json");
            crate::config::save_to(&PeriConfig::default(), &target).unwrap();
            target
        } else {
            global.clone()
        };
        let original_global = std::fs::read(&global).unwrap();
        let source = Arc::new(ConfigSource::load_at(&cwd, global.clone()).unwrap());
        let config = Arc::new(parking_lot::RwLock::new(source.loaded_merged()));
        // Use the effective initial alias even if the workspace default overrides it.
        config.write().config.active_alias = "sonnet".into();
        assert!(PERI_CONFIG_HANDLE.set(config.clone()).is_ok());
        assert!(CONFIG_SOURCE_HANDLE.set(source).is_ok());
        *SERVICE_SNAPSHOT.state().write() = ServiceSnapshot {
            cwd: "preserved-cwd".into(),
            provider_name: "provider-before".into(),
            model_alias: "sonnet".into(),
            model_name: "a-sonnet".into(),
            effort: "xhigh".into(),
            memory_mb: 42,
            cron_total: 3,
            ..Default::default()
        };
        let (transport, server) = mpsc_transport_pair();
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
            config,
            client,
            server,
            notifications,
        }
    }

    async fn recv(&self) -> IncomingMessage {
        tokio::time::timeout(Duration::from_secs(3), self.server.recv())
            .await
            .expect("model action must reach real transport")
            .unwrap()
    }

    async fn create_session(&self) {
        let client = self.client.clone();
        let task = tokio::spawn(async move { client.new_session(".", None).await });
        let IncomingMessage::Request { id, method, .. } = self.recv().await else {
            panic!("expected new request")
        };
        assert_eq!(method, "session/new");
        self.server
            .send_response(id, Ok(json!({"sessionId":"model-session"})))
            .await
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap(), "model-session");
    }

    fn assert_disk_and_frame(&self, params: &Value) {
        assert!(
            self.config.try_write().is_some(),
            "config lock must be released before RPC"
        );
        let expected = self.config.read().clone();
        let wire: PeriConfig = serde_json::from_value(params["config"].clone()).unwrap();
        assert_eq!(wire, expected);
        let loaded = ConfigSource::load_at(&self.cwd, self.global.clone()).unwrap();
        assert_eq!(
            loaded.loaded_merged(),
            expected,
            "save must precede RPC frame"
        );
    }

    /// 断言一次成功动作的推送帧：磁盘、内存与线上配置三者一致，且不带 sessionId。
    /// `fail_rpc` 让 host 侧回错误，用于断言推送失败不回滚也不重发。
    async fn expect_request(&self, fail_rpc: bool) {
        let IncomingMessage::Request { id, method, params } = self.recv().await else {
            panic!("expected update request")
        };
        assert_eq!(method, "session/update_config");
        assert!(
            params.get("sessionId").is_none(),
            "complete host configuration must not target the active workspace"
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
            .unwrap();
    }

    /// 保存成功且推送成功的一次动作。
    async fn expect_notification(&self) {
        self.expect_request(false).await;
    }

    async fn finish(mut self) {
        self.client.close();
        // The production pump is detached, so observe its terminal channel boundary.
        // Do not claim a JoinHandle the API does not provide.
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.notifications.recv().await.is_some() {}
        })
        .await
        .expect("notification pump must stop after close");
        assert!(
            tokio::time::timeout(Duration::from_secs(3), self.server.recv())
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn model_commit_switch_persists_workspace_before_request() {
    isolated(
        "model_commit_switch_persists_workspace_before_request",
        async {
            let fixture = Fixture::new(true);
            fixture.create_session().await;
            let before = SERVICE_SNAPSHOT.state().read().clone();
            switch_active_alias(3);
            let snapshot = SERVICE_SNAPSHOT.state().read().clone();
            assert_eq!(
                snapshot, before,
                "host edits must not overwrite the active session projection"
            );
            fixture.expect_request(false).await;
            assert_eq!(
                std::fs::read(&fixture.global).unwrap(),
                fixture.original_global
            );
            // Re-selecting the active alias still pushes the same saved snapshot.
            switch_active_alias(3);
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

#[test]
fn model_commit_active_provider_updates_mapping_and_request() {
    isolated(
        "model_commit_active_provider_updates_mapping_and_request",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            edit_field("sonnet".into(), FIELD_PROVIDER, true);
            let profile = fixture.config.read().config.profiles.sonnet.clone();
            assert_eq!(profile.provider, "b");
            assert_eq!(profile.model.as_deref(), Some("b-sonnet"));
            assert_eq!(profile.max_tokens, 32000);
            assert!(!profile.context_1m);
            let snapshot = SERVICE_SNAPSHOT.state().read().clone();
            assert_ne!(
                snapshot.model_name, "b-sonnet",
                "editing host settings must not change the session model"
            );
            assert_eq!(snapshot.memory_mb, 42);
            assert!(MODEL_HIGHLIGHT_UNTIL.get().is_none());
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

/// Responses provider 与其它 provider 共用即时保存：Provider 行切换后立刻落盘，
/// 保留已确认的 contextWindow，不经过“档位配置草稿”。
#[test]
fn model_commit_responses_provider_switch_persists_immediately() {
    isolated(
        "model_commit_responses_provider_switch_persists_immediately",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            {
                let mut cfg = fixture.config.write();
                cfg.config.profiles.sonnet.provider = "a".into();
                cfg.config.profiles.sonnet.model = None;
                cfg.config.profiles.sonnet.context_window = Some(3000);
            }
            // providers 顺序 a、b、r；从 a 后退即 r。
            edit_field("sonnet".into(), FIELD_PROVIDER, false);
            let profile = fixture.config.read().config.profiles.sonnet.clone();
            assert_eq!(profile.provider, "r");
            assert_eq!(profile.model.as_deref(), Some("r-sonnet"));
            assert_eq!(
                profile.context_window,
                Some(3000),
                "即时切换必须保留已确认容量"
            );
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

/// Responses provider 的 Model 行同样即时保存，不打开任何专属面板。
#[test]
fn model_commit_responses_model_switch_persists_immediately() {
    isolated(
        "model_commit_responses_model_switch_persists_immediately",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            {
                let mut cfg = fixture.config.write();
                cfg.config.profiles.sonnet.provider = "r".into();
                cfg.config.profiles.sonnet.model = None;
                cfg.config.profiles.sonnet.context_window = Some(3000);
            }
            edit_field("sonnet".into(), FIELD_MODEL, true);
            let profile = fixture.config.read().config.profiles.sonnet.clone();
            assert_eq!(profile.model.as_deref(), Some("r-haiku"));
            assert_eq!(profile.context_window, Some(3000));
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

/// 截图回归：辅助档位绑定了缺 contextWindow 的 Responses provider 时，
/// 编辑当前生效的非 Responses 档位仍必须保存成功（baseline 已坏不阻断）。
#[test]
fn model_commit_unrelated_broken_responses_tier_does_not_block_save() {
    isolated(
        "model_commit_unrelated_broken_responses_tier_does_not_block_save",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            {
                let mut cfg = fixture.config.write();
                cfg.config.active_alias = "sonnet".into();
                cfg.config.profiles.sonnet.provider = "a".into();
                cfg.config.profiles.sonnet.model = Some("a-sonnet".into());
                cfg.config.profiles.sonnet.context_window = None;
                cfg.config.profiles.haiku.provider = "r".into();
                cfg.config.profiles.haiku.model = Some("r-haiku".into());
                cfg.config.profiles.haiku.context_window = None;
            }
            edit_field("sonnet".into(), FIELD_MODEL, true);
            let cfg = fixture.config.read().clone();
            assert_eq!(
                cfg.config.profiles.sonnet.model.as_deref(),
                Some("a-haiku"),
                "无关编辑必须落盘"
            );
            assert_eq!(cfg.config.profiles.haiku.provider, "r");
            assert_eq!(
                cfg.config.profiles.haiku.context_window, None,
                "不得为修好其它档位而擅自改坏档位的窗口"
            );
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

#[test]
fn model_commit_inactive_edit_preserves_display_and_notifies() {
    isolated(
        "model_commit_inactive_edit_preserves_display_and_notifies",
        async {
            let fixture = Fixture::new(false);
            let before = SERVICE_SNAPSHOT.state().read().clone();
            edit_field("haiku".into(), FIELD_EFFORT, true);
            assert_eq!(fixture.config.read().config.profiles.haiku.effort, "max");
            assert_eq!(fixture.config.read().config.active_alias, "sonnet");
            assert_eq!(SERVICE_SNAPSHOT.state().read().clone(), before);
            fixture.expect_notification().await;
            edit_field("haiku".into(), FIELD_EFFORT, false);
            assert_eq!(fixture.config.read().config.profiles.haiku.effort, "xhigh");
            assert_eq!(SERVICE_SNAPSHOT.state().read().clone(), before);
            fixture.expect_notification().await;
            fixture.finish().await;
        },
    );
}

#[test]
fn model_commit_save_failure_keeps_memory_and_pushes_nothing() {
    isolated(
        "model_commit_save_failure_keeps_memory_and_pushes_nothing",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            // 让保存目标不可写：把配置文件换成同名目录
            std::fs::remove_file(&fixture.target).unwrap();
            std::fs::create_dir(&fixture.target).unwrap();
            edit_field("sonnet".into(), FIELD_CONTEXT_1M, true);
            assert!(
                !fixture.config.read().config.profiles.sonnet.context_1m,
                "保存失败不得留下「已切换」的内存假象"
            );
            let failed_notice = NOTIFICATION
                .state()
                .read()
                .as_ref()
                .unwrap()
                .message
                .clone();
            assert!(
                failed_notice.contains("Configuration save failed:"),
                "{failed_notice}"
            );
            // 失败的动作不得推送未落盘的配置。子进程跑在 current-thread runtime 上：
            // 推送任务只能在本 await 里被驱动，因此「等待帧通道」足以判定没有推送。
            assert!(
                tokio::time::timeout(Duration::from_millis(500), fixture.server.recv())
                    .await
                    .is_err(),
                "保存失败不得推送配置"
            );
            fixture.finish().await;
            assert!(
                !PERI_CONFIG_HANDLE
                    .get()
                    .unwrap()
                    .read()
                    .config
                    .profiles
                    .sonnet
                    .context_1m
            );
            assert_eq!(
                NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .unwrap()
                    .message
                    .clone(),
                failed_notice
            );
        },
    );
}

/// 推送失败（host 返回错误）不回滚已落盘的内存，也不重发——推送只是发布，
/// 落盘结果才是面板显示的依据。
#[test]
fn model_commit_rpc_failure_keeps_saved_state_without_retry() {
    isolated(
        "model_commit_rpc_failure_keeps_saved_state_without_retry",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            edit_field("sonnet".into(), FIELD_CONTEXT_1M, true);
            assert!(fixture.config.read().config.profiles.sonnet.context_1m);
            fixture.expect_request(true).await;
            fixture.finish().await;
            assert!(
                PERI_CONFIG_HANDLE
                    .get()
                    .unwrap()
                    .read()
                    .config
                    .profiles
                    .sonnet
                    .context_1m
            );
        },
    );
}

#[test]
fn model_commit_context_window_editor_writes_one_profile_and_frame() {
    isolated(
        "model_commit_context_window_editor_writes_one_profile_and_frame",
        async {
            let fixture = Fixture::new(false);
            let before_other = {
                let mut cfg = fixture.config.write();
                cfg.config.profiles.sonnet.provider = "r".into();
                cfg.config.profiles.sonnet.context_window = Some(3000);
                cfg.config.profiles.sonnet.model = None;
                cfg.config.profiles.haiku.clone()
            };
            let revision = crate::config::configuration_revision().unwrap();
            let cfg = fixture.config.read().clone();
            let mut editor = WindowEditState::enter(&cfg, "sonnet", Some(revision)).unwrap();
            editor.text = "9000".into();
            editor.cursor = 4;
            assert!(submit_window_edit(&mut editor));

            let saved = fixture.config.read().clone();
            assert_eq!(saved.config.active_alias, "sonnet");
            assert_eq!(saved.config.profiles.sonnet.context_window, Some(9000));
            assert_eq!(
                saved.config.profiles.haiku, before_other,
                "单档 window 编辑不得改动其它档"
            );
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

#[test]
fn model_commit_context_window_failure_keeps_memory_and_pushes_nothing() {
    isolated(
        "model_commit_context_window_failure_keeps_memory_and_pushes_nothing",
        async {
            let fixture = Fixture::new(false);
            let before = {
                let mut cfg = fixture.config.write();
                cfg.config.profiles.sonnet.provider = "r".into();
                cfg.config.profiles.sonnet.context_window = Some(3000);
                cfg.clone()
            };
            let revision = crate::config::configuration_revision().unwrap();
            let cfg = fixture.config.read().clone();
            let mut editor = WindowEditState::enter(&cfg, "sonnet", Some(revision)).unwrap();
            editor.text = "9000".into();
            std::fs::remove_file(&fixture.target).unwrap();
            std::fs::create_dir(&fixture.target).unwrap();

            assert!(!submit_window_edit(&mut editor));
            assert!(editor.error.is_some(), "失败必须保留字段错误");
            assert_eq!(*fixture.config.read(), before);
            fixture.finish().await;
        },
    );
}

#[test]
fn model_commit_window_editor_input_error_blocks_submit_until_corrected() {
    isolated(
        "model_commit_window_editor_input_error_blocks_submit_until_corrected",
        async {
            let fixture = Fixture::new(false);
            {
                let mut cfg = fixture.config.write();
                cfg.config.profiles.sonnet.provider = "r".into();
                cfg.config.profiles.sonnet.context_window = Some(3000);
                cfg.config.profiles.sonnet.model = None;
            }
            let revision = crate::config::configuration_revision().unwrap();
            let cfg = fixture.config.read().clone();
            let mut editor = WindowEditState::enter(&cfg, "sonnet", Some(revision)).unwrap();
            assert_eq!(editor.text, "3000");

            assert!(!editor.insert('k'), "非数字输入必须被拒绝");
            assert!(editor.input_blocked);
            let before = fixture.config.read().clone();
            assert!(
                !submit_window_edit(&mut editor),
                "被拒绝的输入未清除前不得提交"
            );
            assert_eq!(*fixture.config.read(), before, "拒绝提交不得改动内存配置");
            assert_eq!(
                editor.error.as_deref(),
                Some(crate::i18n::tr("model-panel-window-invalid").as_str())
            );

            editor.backspace();
            assert!(!editor.input_blocked, "实际编辑后错误生命周期结束");
            assert!(submit_window_edit(&mut editor));
            assert_eq!(
                fixture.config.read().config.profiles.sonnet.context_window,
                Some(300)
            );
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

#[test]
fn model_commit_mouse_window_editor_save_persists_and_pushes() {
    isolated(
        "model_commit_mouse_window_editor_save_persists_and_pushes",
        async {
            let fixture = Fixture::new(false);
            {
                let mut config = fixture.config.write();
                config.config.profiles.sonnet.provider = "r".into();
                config.config.profiles.sonnet.context_window = Some(3000);
                config.config.profiles.sonnet.model = None;
            }
            let revision = crate::config::configuration_revision().unwrap();
            let cfg = fixture.config.read().clone();
            let mut editor = WindowEditState::enter(&cfg, "sonnet", Some(revision)).unwrap();
            editor.text = "64000".into();
            editor.cursor = editor.text.chars().count();
            let layout = super::test_window_layout(&editor);
            let row = super::pane_click_row(super::TEST_AREA, &layout, super::PaneHit::Save, 0);
            let column = super::right_content_column(super::TEST_AREA);
            let clicked = std::rc::Rc::new(std::cell::RefCell::new(None));
            let clicked_for_event = std::rc::Rc::clone(&clicked);
            let mut pending = Some(editor);

            super::mount_panel_frames(super::TEST_AREA, 1, move |_frame, panel| {
                if let Some(editor) = pending.take() {
                    *panel.window_editor.write() = Some(editor);
                    let result = panel.event(
                        super::left_down(row, column),
                        super::TEST_AREA,
                        &clicked_for_event,
                    );
                    assert_eq!(result, ratatui_kit::prelude::EventResult::Consumed);
                    assert!(
                        panel.window_editor.read().is_none(),
                        "保存成功后必须关闭内联 editor"
                    );
                }
            });

            assert!(clicked.borrow().is_none(), "窗口保存不得切换 active alias");
            assert_eq!(
                fixture.config.read().config.profiles.sonnet.context_window,
                Some(64000)
            );

            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

#[test]
fn model_commit_window_editor_paste_rejected_until_corrected() {
    isolated(
        "model_commit_window_editor_paste_rejected_until_corrected",
        async {
            let fixture = Fixture::new(false);
            {
                let mut cfg = fixture.config.write();
                cfg.config.profiles.sonnet.provider = "r".into();
                cfg.config.profiles.sonnet.context_window = Some(3000);
                cfg.config.profiles.sonnet.model = None;
            }
            let revision = crate::config::configuration_revision().unwrap();
            let cfg = fixture.config.read().clone();
            let mut editor = WindowEditState::enter(&cfg, "sonnet", Some(revision)).unwrap();
            assert_eq!(editor.text, "3000");

            assert!(!editor.paste("12\n34"), "多行粘贴必须被拒绝");
            assert_eq!(editor.text, "3000", "被拒绝的粘贴不得写入");
            assert_eq!(
                editor.error.as_deref(),
                Some(crate::i18n::tr("model-panel-window-invalid").as_str()),
                "拒绝必须留下可见错误"
            );
            assert!(editor.input_blocked);
            let before = fixture.config.read().clone();
            assert!(
                !submit_window_edit(&mut editor),
                "被拒绝的输入未修正前不得提交"
            );
            assert_eq!(*fixture.config.read(), before, "拒绝提交不得改内存配置");

            editor.backspace();
            assert!(!editor.input_blocked, "修正后错误生命周期结束");
            assert!(editor.paste("9"), "纯数字粘贴可接受");
            assert_eq!(editor.text, "3009");
            assert!(submit_window_edit(&mut editor));
            assert_eq!(
                fixture.config.read().config.profiles.sonnet.context_window,
                Some(3009)
            );
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}
