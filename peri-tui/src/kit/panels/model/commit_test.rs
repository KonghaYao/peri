//! Real model actions, layered configuration files and ACP MPSC frames.
//! Each scenario owns a process because the production handles are OnceLocks.
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
    notifications: tokio::sync::mpsc::UnboundedReceiver<AcpNotification>,
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

    async fn expect_request(&self, save_succeeded: bool, fail_rpc: bool) {
        let IncomingMessage::Request { id, method, params } = self.recv().await else {
            panic!("expected update request")
        };
        assert_eq!(method, "session/update_config");
        assert_eq!(params["sessionId"], "model-session");
        if save_succeeded {
            self.assert_disk_and_frame(&params);
        } else {
            assert!(self.config.try_write().is_some());
            let wire: PeriConfig = serde_json::from_value(params["config"].clone()).unwrap();
            assert_eq!(wire, *self.config.read());
        }
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

    async fn expect_notification(&self) {
        let IncomingMessage::Notification { method, params } = self.recv().await else {
            panic!("expected config notification")
        };
        assert_eq!(method, "session/config_update");
        assert!(params.get("sessionId").is_none());
        self.assert_disk_and_frame(&params);
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
            assert_eq!(snapshot.model_alias, "haiku");
            assert_eq!(snapshot.model_name, "a-haiku");
            assert_eq!(snapshot.effort, "xhigh");
            assert_eq!(snapshot.provider_name, before.provider_name);
            assert_eq!(
                (snapshot.cwd, snapshot.memory_mb, snapshot.cron_total),
                (before.cwd, 42, 3)
            );
            assert!(
                MODEL_HIGHLIGHT_UNTIL
                    .get()
                    .is_some_and(|until| until > Instant::now())
            );
            fixture.expect_request(true, false).await;
            assert_eq!(
                std::fs::read(&fixture.global).unwrap(),
                fixture.original_global
            );
            // Selecting the active alias still saves and pushes the same snapshot.
            std::fs::remove_file(&fixture.target).unwrap();
            switch_active_alias(3);
            fixture.expect_request(true, false).await;
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
            assert_eq!(
                (
                    snapshot.model_alias.as_str(),
                    snapshot.model_name.as_str(),
                    snapshot.provider_name.as_str()
                ),
                ("sonnet", "b-sonnet", "anthropic")
            );
            assert_eq!(snapshot.memory_mb, 42);
            assert!(MODEL_HIGHLIGHT_UNTIL.get().is_none());
            fixture.expect_request(true, false).await;
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
fn model_commit_save_failure_keeps_memory_and_still_requests() {
    isolated(
        "model_commit_save_failure_keeps_memory_and_still_requests",
        async {
            let fixture = Fixture::new(false);
            fixture.create_session().await;
            std::fs::remove_file(&fixture.target).unwrap();
            std::fs::create_dir(&fixture.target).unwrap();
            edit_field("sonnet".into(), FIELD_CONTEXT_1M, true);
            assert!(fixture.config.read().config.profiles.sonnet.context_1m);
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
            fixture.expect_request(false, true).await;
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
