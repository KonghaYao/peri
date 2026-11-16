//! Tests for service_snapshot

use super::ACTIVE_EXECUTION_CWD;
use super::ACTIVE_SESSION_ID;
use super::AcpTuiClient;
use super::Arc;
use super::CancellationToken;
use super::Duration;
use super::Mutex;
use super::SlowSnapshotRefresh;
use super::SnapshotSource;
use super::THREAD_BROWSER_ARCHIVED;
use super::THREAD_BROWSER_SCOPE;
use super::THREAD_LIST_ERROR;
use super::THREAD_LIST_PAGE_COUNT;
use super::file_scan;
use super::memory_scan;

use crate::app::service_registry::ProcessResourceMonitor;

use chrono::Utc;
use peri_acp::transport::AcpTransport;
use peri_acp::transport::types::IncomingMessage;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use serde_json::{Value, json};
use serial_test::serial;
use std::time::Instant;

fn reset_snapshot_atoms() {
    crate::kit::atoms::init_atoms();
    ACTIVE_EXECUTION_CWD.set(None);
    ACTIVE_SESSION_ID.set(String::new());
    THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Project);
    THREAD_BROWSER_ARCHIVED.set(false);
    THREAD_LIST_PAGE_COUNT.set(1);
    THREAD_LIST_ERROR.set(None);
    THREAD_LIST.state().write().clear();
    SERVICE_PROJECTION_ERROR.set(None);
}

fn isolated_refresh() -> SlowSnapshotRefresh {
    SlowSnapshotRefresh {
        memory: memory_scan::MemoryScan {
            next: Instant::now() + Duration::from_secs(3600),
            entries: Vec::new(),
        },
        ..SlowSnapshotRefresh::default()
    }
}

fn workspace_json(cwd: &str) -> Value {
    json!({
        "project_id":"00000000-0000-0000-0000-000000000001",
        "workspace_id":"00000000-0000-0000-0000-000000000002",
        "cwd":cwd,"root":cwd,"relative_cwd":""
    })
}

fn entry_json(cwd: &str, title: &str) -> Value {
    json!({
        "thread": {"id":"00000000-0000-0000-0000-000000000003", "cwd":"/creation/path",
            "title":title,"message_count":3,"updated_at":"2026-09-12T00:00:00Z"},
        "binding": {"schema_version":1,"revision":1,
            "project_id":"00000000-0000-0000-0000-000000000001",
            "workspace_id":"00000000-0000-0000-0000-000000000002",
            "cwd_relative_to_workspace":""},
        "effective_cwd":cwd,"workspace_root":cwd
    })
}

async fn snapshot_client(cwd: &str) -> (AcpTuiClient, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("00000000-0000-0000-0000-000000000003", false);
    let cwd = cwd.to_string();
    let (queries, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(IncomingMessage::Request { id, method, params }) = server.recv().await {
            let result = match method.as_str() {
                "initialize" => {
                    json!({"agentCapabilities":{"_meta":{"peri.sessionWorkspaceV1":true}}})
                }
                "peri/session_context" => json!({"version":1,"workspace":workspace_json(&cwd),
                    "binding":entry_json(&cwd,"title")["binding"],"title":"Session title"}),
                "session/metadata" => {
                    json!({"title":"Session title","permissionMode":"accept_edit","modelAlias":"workspace-model"})
                }
                "plugin/list" => {
                    assert_eq!(params["sessionId"], "00000000-0000-0000-0000-000000000003");
                    json!({"plugins":[],"hooks":[{"event":"pretooluse","plugin_name":"target-hooks","command":"echo target","matcher":null}]})
                }
                "mcp/list" => {
                    assert_eq!(params["sessionId"], "00000000-0000-0000-0000-000000000003");
                    json!({"servers":[{"name":"target-mcp","transport":"stdio","connectionStatus":"connected","oauthStatus":"none","toolsCount":2}]})
                }
                "cron/list" => {
                    json!({"jobs":[{"id":"job-1","expression":"*/5 * * * *","prompt":"test prompt","enabled":true,"next_fire":null}]})
                }
                "session/list" => {
                    queries.send(params).expect(
                        "snapshot client: fixture channel must deliver its expected message",
                    );
                    json!({"sessions":[],"_meta":{"peri.sessionWorkspaceV1":{
                        "threads":[entry_json(&cwd,"Project thread")],"nextCursor":null}}})
                }
                other => panic!("unexpected request: {other}"),
            };
            server
                .send_response(id, Ok(result))
                .await
                .expect("snapshot client: fixture task and request must complete successfully");
        }
    });
    client
        .register_ui_commands(&[])
        .await
        .expect("snapshot client: fixture task and request must complete successfully");
    (client, rx)
}

#[tokio::test]
#[serial]
async fn project_history_uses_host_scope_and_actual_execution_path() {
    reset_snapshot_atoms();
    let tmp = tempfile::tempdir().expect("project history uses host scope and actual execution path: isolated fixture workspace must remain accessible");
    let cwd = tmp.path().to_str().expect("project history uses host scope and actual execution path: fixture paths and protocol text must be valid UTF-8");
    let (client, mut queries) = snapshot_client(cwd).await;
    let mut src = make_minimal_source(Some(client));
    src.cwd = cwd.into();
    let mut slow = isolated_refresh();
    super::tick_once(&src, &mut slow).await.expect("project history uses host scope and actual execution path: fixture task and request must complete successfully");
    assert_eq!(*THREAD_LIST_ERROR.state().read(), None);
    let project_query = tokio::time::timeout(Duration::from_secs(3), queries.recv())
        .await
        .expect("project scope query must be sent")
        .expect("project history uses host scope and actual execution path: fixture task and request must complete successfully");
    assert_eq!(
        project_query["_meta"]["peri.sessionWorkspaceV1"]["scope"]["kind"],
        "project"
    );
    let threads = THREAD_LIST.state().read().clone();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].cwd, cwd);
    assert_eq!(threads[0].message_count, 3);
    THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Workspace);
    super::tick_once(&src, &mut slow).await.expect("project history uses host scope and actual execution path: fixture task and request must complete successfully");
    let workspace_query = tokio::time::timeout(Duration::from_secs(3), queries.recv())
        .await
        .expect("workspace scope query must be sent")
        .expect("project history uses host scope and actual execution path: fixture task and request must complete successfully");
    assert_eq!(
        workspace_query["_meta"]["peri.sessionWorkspaceV1"]["scope"]["kind"],
        "workspace"
    );
}

#[tokio::test]
#[serial]
async fn active_execution_directory_invalidates_file_cache_and_updates_title() {
    reset_snapshot_atoms();
    let original = tempfile::tempdir().expect("active execution directory invalidates file cache and updates title: isolated fixture workspace must remain accessible");
    let target = tempfile::tempdir().expect("active execution directory invalidates file cache and updates title: isolated fixture workspace must remain accessible");
    std::fs::write(original.path().join("original.rs"), "old").expect("active execution directory invalidates file cache and updates title: isolated fixture workspace must remain accessible");
    std::fs::write(target.path().join("target.rs"), "new").expect("active execution directory invalidates file cache and updates title: isolated fixture workspace must remain accessible");
    let (client, _queries) = snapshot_client(target.path().to_str().expect("active execution directory invalidates file cache and updates title: fixture paths and protocol text must be valid UTF-8")).await;
    let mut src = make_minimal_source(Some(client));
    src.cwd = original.path().to_str().expect("active execution directory invalidates file cache and updates title: fixture paths and protocol text must be valid UTF-8").into();
    let mut slow = isolated_refresh();
    super::tick_once(&src, &mut slow).await.expect("active execution directory invalidates file cache and updates title: fixture task and request must complete successfully");
    assert_eq!(*FILE_LIST.state().read(), vec!["original.rs"]);
    ACTIVE_EXECUTION_CWD.set(Some(target.path().to_str().expect("active execution directory invalidates file cache and updates title: fixture paths and protocol text must be valid UTF-8").into()));
    ACTIVE_SESSION_ID.set("00000000-0000-0000-0000-000000000003".into());
    super::tick_once(&src, &mut slow).await.expect("active execution directory invalidates file cache and updates title: fixture task and request must complete successfully");
    assert_eq!(*FILE_LIST.state().read(), vec!["target.rs"]);
    assert_eq!(
        SERVICE_SNAPSHOT.state().read().cwd,
        target.path().to_str().expect("active execution directory invalidates file cache and updates title: fixture paths and protocol text must be valid UTF-8")
    );
    assert_eq!(*CURRENT_SESSION_TITLE.state().read(), "Session title");
    assert_eq!(
        SERVICE_SNAPSHOT.state().read().permission_mode,
        "accept-edit"
    );
    assert_eq!(
        SERVICE_SNAPSHOT.state().read().model_alias,
        "workspace-model"
    );
    assert_eq!(HOOK_LIST.state().read()[0].plugin_name, "target-hooks");
    assert_eq!(MCP_SERVERS.state().read()[0].name, "target-mcp");
    ACTIVE_EXECUTION_CWD.set(None);
    ACTIVE_SESSION_ID.set(String::new());
}

#[tokio::test]
#[serial]
async fn closing_session_clears_session_projection_on_next_commit() {
    reset_snapshot_atoms();
    let directory = tempfile::tempdir().expect("closing session clears session projection on next commit: isolated fixture workspace must remain accessible");
    let (client, _queries) = snapshot_client(directory.path().to_str().expect("closing session clears session projection on next commit: fixture paths and protocol text must be valid UTF-8")).await;
    let mut source = make_minimal_source(Some(client));
    source.cwd = directory.path().to_str().expect("closing session clears session projection on next commit: fixture paths and protocol text must be valid UTF-8").into();
    let mut refresh = isolated_refresh();
    ACTIVE_SESSION_ID.set("00000000-0000-0000-0000-000000000003".into());
    super::tick_once(&source, &mut refresh).await.expect("closing session clears session projection on next commit: fixture task and request must complete successfully");
    assert_eq!(*CURRENT_SESSION_TITLE.state().read(), "Session title");
    assert!(!CRON_JOBS.state().read().is_empty());
    ACTIVE_SESSION_ID.set(String::new());
    super::tick_once(&source, &mut refresh).await.expect("closing session clears session projection on next commit: fixture task and request must complete successfully");
    assert!(CURRENT_SESSION_TITLE.state().read().is_empty());
    assert!(HOOK_LIST.state().read().is_empty());
    assert!(PLUGIN_LIST.state().read().is_empty());
    assert!(MCP_SERVERS.state().read().is_empty());
    assert!(CRON_JOBS.state().read().is_empty());
    let snapshot = SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snapshot.cron_total, 0);
    assert_eq!(snapshot.cron_enabled, 0);
    assert_eq!(snapshot.mcp.init_phase, McpInitPhase::Failed);
    assert_eq!(snapshot.model_alias, "");
    assert_eq!(snapshot.permission_mode, "default");
    assert_eq!(snapshot.effort, "xhigh");
}

#[tokio::test]
#[serial]
async fn missing_workspace_capability_is_visible_as_query_error() {
    reset_snapshot_atoms();
    let (transport, _server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    let src = make_minimal_source(Some(client));
    super::tick_once(&src, &mut isolated_refresh()).await.expect("missing workspace capability is visible as query error: fixture task and request must complete successfully");
    assert!(
        THREAD_LIST_ERROR
            .state()
            .read()
            .clone()
            .expect("missing workspace capability is visible as query error: the fixture storage and layout must support the requested operation")
            .contains("does not support")
    );
    assert!(THREAD_LIST.state().read().is_empty());
}

fn make_minimal_source(client: Option<AcpTuiClient>) -> SnapshotSource {
    let peri_config = Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    ));
    let permission_mode = SharedPermissionMode::new(PermissionMode::Default);
    let monitor = Arc::new(Mutex::new(ProcessResourceMonitor::new()));

    SnapshotSource {
        cwd: ".".into(),
        client,
        peri_config,
        permission_mode,
        resource_monitor: monitor,
        hooks: Vec::new(),
        plugins: Vec::new(),
        providers: Vec::new(),
    }
}

#[tokio::test]
#[serial]
async fn test_tick_once_writes_atoms() {
    // 先 init atoms（避免 SERVICE_SNAPSHOT.get() 返回 None）
    reset_snapshot_atoms();

    let src = make_minimal_source(None);
    let mut slow = isolated_refresh();
    let result = super::tick_once(&src, &mut slow).await;
    assert!(result.is_ok(), "tick_once should succeed");

    let snap = SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cwd, ".");
    assert_eq!(snap.cron_total, 0);
    assert_eq!(snap.cron_enabled, 0);
    assert_eq!(snap.mcp.total, 0);
    assert_eq!(snap.mcp.connected, 0);
    assert_eq!(snap.mcp.init_phase, McpInitPhase::Failed);
    assert!(SERVICE_PROJECTION_ERROR.state().read().is_some());
}

#[tokio::test]
#[serial]
async fn test_tick_once_empty_thread_list() {
    reset_snapshot_atoms();

    let src = make_minimal_source(None);
    // ACP 项目列表为空时不生成会话行
    let mut slow = isolated_refresh();
    let result = super::tick_once(&src, &mut slow).await;
    assert!(result.is_ok());

    let threads = THREAD_LIST.state().read().clone();
    assert!(threads.is_empty());
}

#[tokio::test]
#[serial]
async fn no_session_projection_preserves_only_labeled_startup_plugins_and_hooks() {
    reset_snapshot_atoms();
    let mut source = make_minimal_source(None);
    source.hooks.push(HookSummary {
        plugin_name: "startup-only".into(),
        ..Default::default()
    });
    super::tick_once(&source, &mut isolated_refresh()).await.expect("no session projection preserves only labeled startup plugins and hooks: fixture task and request must complete successfully");
    assert_eq!(HOOK_LIST.state().read()[0].plugin_name, "startup-only");
    assert!(CRON_JOBS.state().read().is_empty());
    assert!(MCP_SERVERS.state().read().is_empty());
    assert_eq!(
        SERVICE_PROJECTION_ERROR.state().read().as_deref(),
        Some(crate::i18n::tr("service-projection-startup").as_str())
    );
    ACTIVE_SESSION_ID.set("session-without-client".into());
    super::tick_once(&source, &mut isolated_refresh()).await.expect("no session projection preserves only labeled startup plugins and hooks: fixture task and request must complete successfully");
    assert!(HOOK_LIST.state().read().is_empty());
    assert!(PLUGIN_LIST.state().read().is_empty());
    assert_eq!(
        SERVICE_PROJECTION_ERROR.state().read().as_deref(),
        Some(crate::i18n::tr("service-projection-no-client").as_str())
    );
}

#[tokio::test]
#[serial]
async fn test_cron_tasks_collected() {
    reset_snapshot_atoms();

    let tmp = tempfile::tempdir()
        .expect("cron tasks collected: isolated fixture workspace must remain accessible");
    let (client, _queries) = snapshot_client(
        tmp.path()
            .to_str()
            .expect("cron tasks collected: fixture paths and protocol text must be valid UTF-8"),
    )
    .await;
    let src = make_minimal_source(Some(client));
    ACTIVE_SESSION_ID.set("00000000-0000-0000-0000-000000000003".into());

    let mut slow = isolated_refresh();
    let result = super::tick_once(&src, &mut slow).await;
    assert!(result.is_ok());

    let jobs = CRON_JOBS.state().read().clone();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, "job-1");

    let snap = SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cron_total, 1);
    assert_eq!(snap.cron_enabled, 1);
}

#[tokio::test]
async fn test_derive_provider_and_model_default() {
    let peri_config = Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    ));
    let (provider, alias, model_name, effort) = super::derive_provider_and_model(&peri_config);
    // 默认 AppConfig:
    assert!(provider.is_empty());
    assert!(alias.is_empty());
    assert!(model_name.is_empty());
    // 无 active profile 时 effort 回退默认档位
    assert_eq!(effort, "xhigh");
}

#[tokio::test]
async fn test_derive_provider_and_model_set() {
    use peri_acp::provider::config::{AppConfig, ProviderConfig, ProviderModels};

    let cfg = crate::config::PeriConfig {
        config: AppConfig {
            active_alias: "sonnet".into(),
            profiles: {
                let mut profiles = crate::config::Profiles::default();
                profiles
                    .get_mut("sonnet")
                    .expect(
                        "derive provider and model set: fixture must contain the requested entry",
                    )
                    .provider = "p1".into();
                profiles
                    .get_mut("sonnet")
                    .expect(
                        "derive provider and model set: fixture must contain the requested entry",
                    )
                    .effort = "high".into();
                profiles
            },
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                models: ProviderModels {
                    opus: "claude-opus-4-20250514".into(),
                    sonnet: "claude-sonnet-4-20250514".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let peri_config = Arc::new(parking_lot::RwLock::new(cfg));
    let (provider, alias, model_name, effort) = super::derive_provider_and_model(&peri_config);
    assert_eq!(provider, "anthropic");
    assert_eq!(alias, "sonnet");
    assert_eq!(model_name, "claude-sonnet-4-20250514");
    // sonnet profile 显式设置 effort = high
    assert_eq!(effort, "high");
}

#[tokio::test]
async fn test_derive_provider_and_model_set_empty_model() {
    use peri_acp::provider::config::{AppConfig, ProviderConfig, ProviderModels};

    let cfg = crate::config::PeriConfig {
        config: AppConfig {
            active_alias: "haiku".into(),
            profiles: {
                let mut profiles = crate::config::Profiles::default();
                profiles.get_mut("haiku").expect("derive provider and model set empty model: fixture must contain the requested entry").provider = "p1".into();
                profiles
            },
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                models: ProviderModels {
                    haiku: "".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let peri_config = Arc::new(parking_lot::RwLock::new(cfg));
    let (provider, alias, model_name, effort) = super::derive_provider_and_model(&peri_config);
    assert_eq!(provider, "anthropic");
    assert_eq!(alias, "haiku");
    // Some("") 应被 filter 掉，回退到 active_alias
    assert_eq!(model_name, "haiku");
    assert_eq!(effort, "xhigh");
}

#[tokio::test]
async fn test_derive_provider_and_model_no_models_fallback() {
    use peri_acp::provider::config::{AppConfig, ProviderConfig};

    let cfg = crate::config::PeriConfig {
        config: AppConfig {
            active_alias: "haiku".into(),
            profiles: {
                let mut profiles = crate::config::Profiles::default();
                profiles.get_mut("haiku").expect("derive provider and model no models fallback: fixture must contain the requested entry").provider = "p1".into();
                profiles
            },
            providers: vec![ProviderConfig {
                id: "p1".into(),
                provider_type: "anthropic".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let peri_config = Arc::new(parking_lot::RwLock::new(cfg));
    let (provider, alias, model_name, effort) = super::derive_provider_and_model(&peri_config);
    assert_eq!(provider, "anthropic");
    assert_eq!(alias, "haiku");
    // 无模型映射时回退到 active_alias
    assert_eq!(model_name, "haiku");
    assert_eq!(effort, "xhigh");
}

#[test]
fn test_chrono_datetime_conversion() {
    // 验证 chrono::DateTime<chrono::Utc> 与 ThreadMeta.updated_at 类型一致
    let now = Utc::now();
    let _dt: chrono::DateTime<Utc> = now;
    // 验证 ThreadSummary.updated_at 是 Option<DateTime<Utc>>
    let summary = ThreadSummary {
        id: "x".into(),
        title: None,
        cwd: ".".into(),
        message_count: 0,
        updated_at: Some(now),
    };
    assert!(summary.updated_at.is_some());
}

/// 编译期断言：SnapshotSource 字段全部 Clone（trait bound 验证）。
#[test]
fn test_snapshot_source_is_clone() {
    fn assert_clone<T: Clone>() {}
    assert_clone::<SnapshotSource>();
}

/// C2 回归测试：scan_cwd_files_shallow 在临时目录正确收集文件相对路径。
#[test]
fn test_scan_cwd_files_shallow_collects_files() {
    let tmp = tempfile::tempdir().expect(
        "scan cwd files shallow collects files: isolated fixture workspace must remain accessible",
    );
    let root = tmp.path();

    // 顶层文件
    std::fs::write(root.join("a.txt"), "x").expect(
        "scan cwd files shallow collects files: isolated fixture workspace must remain accessible",
    );
    std::fs::write(root.join("b.rs"), "x").expect(
        "scan cwd files shallow collects files: isolated fixture workspace must remain accessible",
    );
    // 子目录文件（深度=2）
    std::fs::create_dir_all(root.join("src")).expect(
        "scan cwd files shallow collects files: isolated fixture workspace must remain accessible",
    );
    std::fs::write(root.join("src").join("mod.rs"), "x").expect(
        "scan cwd files shallow collects files: isolated fixture workspace must remain accessible",
    );
    // 忽略目录：node_modules 内文件不应出现
    std::fs::create_dir_all(root.join("node_modules").join("pkg")).expect(
        "scan cwd files shallow collects files: isolated fixture workspace must remain accessible",
    );
    std::fs::write(
        root.join("node_modules").join("pkg").join("ignored.js"),
        "x",
    )
    .expect(
        "scan cwd files shallow collects files: isolated fixture workspace must remain accessible",
    );

    let files = crate::kit::service_snapshot::file_scan::scan_cwd_files_shallow(root.to_str().expect("scan cwd files shallow collects files: fixture paths and protocol text must be valid UTF-8"));
    assert!(files.contains(&"a.txt".to_string()));
    assert!(files.contains(&"b.rs".to_string()));
    assert!(files.contains(&"src/mod.rs".to_string()));
    assert!(
        !files.iter().any(|f| f.contains("node_modules")),
        "ignored dir should be filtered out, got: {:?}",
        files
    );
}

/// C2 回归测试：不存在的目录返回空 Vec，不 panic。
#[test]
fn test_scan_cwd_files_shallow_nonexistent() {
    let files = crate::kit::service_snapshot::file_scan::scan_cwd_files_shallow(
        "/this/path/does/not/exist",
    );
    assert!(files.is_empty());
}

/// C2 回归测试：MAX_FILES 上限防止无限增长。
#[test]
fn test_scan_cwd_files_shallow_caps_at_max() {
    let tmp = tempfile::tempdir().expect(
        "scan cwd files shallow caps at max: isolated fixture workspace must remain accessible",
    );
    let root = tmp.path();
    // 创建 600 个文件（超过 MAX_FILES=500）
    for i in 0..600 {
        std::fs::write(root.join(format!("f{i}.txt")), "x").expect(
            "scan cwd files shallow caps at max: isolated fixture workspace must remain accessible",
        );
    }
    let files = crate::kit::service_snapshot::file_scan::scan_cwd_files_shallow(root.to_str().expect("scan cwd files shallow caps at max: fixture paths and protocol text must be valid UTF-8"));
    assert!(files.len() <= 500, "should cap at 500, got {}", files.len());
}

mod startup {
    use super::*;
    use peri_acp::transport::mpsc::MpscServerTransport;
    use peri_acp::transport::types::RequestId;

    const SESSION_ID: &str = "00000000-0000-0000-0000-000000000003";

    async fn request(server: &MpscServerTransport, expected: &str) -> RequestId {
        let message = tokio::time::timeout(Duration::from_millis(10), server.recv())
            .await
            .expect("会话就绪后必须立即刷新，不能等待 2 秒 tick")
            .expect("transport 不应关闭");
        let IncomingMessage::Request { id, method, .. } = message else {
            panic!("应收到请求");
        };
        assert_eq!(method, expected);
        id
    }

    async fn setup() -> (SnapshotSource, MpscServerTransport, tempfile::TempDir) {
        reset_snapshot_atoms();
        SERVICE_SNAPSHOT.set(ServiceSnapshot::default());
        let directory =
            tempfile::tempdir().expect("setup: isolated fixture workspace must remain accessible");
        let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
        let (client, _, _) = AcpTuiClient::new_interactive(transport);
        let initialize = client.clone();
        let task = tokio::spawn(async move { initialize.register_ui_commands(&[]).await });
        let id = request(&server, "initialize").await;
        server
            .send_response(
                id,
                Ok(json!({"agentCapabilities":{"_meta":{
                    "peri.sessionWorkspaceV1":true
                }}})),
            )
            .await
            .expect("setup: fixture task and request must complete successfully");
        task.await
            .expect("setup: fixture task and request must complete successfully")
            .expect("setup: fixture task and request must complete successfully");
        let mut source = make_minimal_source(Some(client));
        source.cwd = directory
            .path()
            .to_str()
            .expect("setup: fixture paths and protocol text must be valid UTF-8")
            .into();
        (source, server, directory)
    }

    fn start(source: SnapshotSource, shutdown: CancellationToken) -> tokio::task::JoinHandle<()> {
        // 外部文件扫描在已有独立测试覆盖；调度测试不读取开发者 HOME，也不依赖磁盘耗时。
        let refresh = SlowSnapshotRefresh {
            files: file_scan::FileScan {
                cwd: source.cwd.clone(),
                next: Instant::now() + Duration::from_secs(3600),
                entries: Vec::new(),
            },
            ..isolated_refresh()
        };
        tokio::spawn(crate::kit::service_snapshot::refresh::run_service_snapshot(
            source, shutdown, refresh,
        ))
    }

    async fn complete_initial_scan(server: &MpscServerTransport, id: RequestId, cwd: &str) {
        server
            .send_response(
                id,
                Ok(json!({
                    "version":1,"workspace":workspace_json(cwd),"binding":null
                })),
            )
            .await
            .expect("complete initial scan: fixture task and request must complete successfully");
        let id = request(server, "session/list").await;
        server
            .send_response(
                id,
                Ok(json!({"sessions":[],"_meta":{
                    "peri.sessionWorkspaceV1":{"threads":[],"nextCursor":null}
                }})),
            )
            .await
            .expect("complete initial scan: fixture task and request must complete successfully");
    }

    async fn new_session(source: &SnapshotSource, server: &MpscServerTransport) {
        let client = source
            .client
            .clone()
            .expect("new session: the clone fixture operation uses valid inputs");
        let cwd = source.cwd.clone();
        let task = tokio::spawn(async move { client.new_session(&cwd, None).await });
        let id = request(server, "session/new").await;
        server
            .send_response(id, Ok(json!({"sessionId":SESSION_ID})))
            .await
            .expect("new session: fixture task and request must complete successfully");
        assert_eq!(
            task.await
                .expect("new session: the fixture operation must finish without an inner failure")
                .expect("new session: the fixture operation must finish without an inner failure"),
            SESSION_ID
        );
    }

    async fn complete_session_snapshot(server: &MpscServerTransport, model: &str) {
        // query 使用 join!，独立服务请求的先后顺序不属于契约。
        for _ in 0..3 {
            let message = tokio::time::timeout(Duration::from_millis(10), server.recv())
                .await
                .expect("必须在周期 tick 前查询会话服务")
                .expect("complete session snapshot: fixture task and request must complete successfully");
            let IncomingMessage::Request { id, method, params } = message else {
                panic!("应收到会话服务请求");
            };
            assert_eq!(params["sessionId"], SESSION_ID);
            let value = match method.as_str() {
                "plugin/list" => json!({"plugins":[],"hooks":[]}),
                "mcp/list" => json!({"servers":[]}),
                "cron/list" => json!({"jobs":[]}),
                other => panic!("未知请求：{other}"),
            };
            server.send_response(id, Ok(value)).await.expect(
                "complete session snapshot: fixture task and request must complete successfully",
            );
        }
        let id = request(server, "session/metadata").await;
        server
            .send_response(
                id,
                Ok(json!({
                    "permissionMode":"bypass","modelAlias":"opus","modelName":model,"effort":"high"
                })),
            )
            .await
            .expect(
                "complete session snapshot: fixture task and request must complete successfully",
            );
        // 暂停时钟下让所有 ready future 完成，仅推进 1 ms，不触发 2 秒周期。
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    /// [回归测试] 首次快照跨越 session commit 被丢弃后，应消费就绪通知立即重采。
    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_startup_snapshot_discard_refreshes_before_periodic_tick() {
        let (source, server, _directory) = setup().await;
        let shutdown = CancellationToken::new();
        let task = start(source.clone(), shutdown.clone());
        let first = request(&server, "peri/session_context").await;
        new_session(&source, &server).await;
        complete_initial_scan(&server, first, &source.cwd).await;
        complete_session_snapshot(&server, "stale-model").await;
        assert!(
            SERVICE_SNAPSHOT.state().read().model_name.is_empty(),
            "目录身份变化后，旧采样不得发布"
        );
        complete_session_snapshot(&server, "ready-model").await;
        let snapshot = SERVICE_SNAPSHOT.state().read().clone();
        assert_eq!(snapshot.model_name, "ready-model");
        assert_eq!(snapshot.permission_mode, "bypass");
        assert_eq!(snapshot.cwd, source.cwd);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), server.recv())
                .await
                .is_err(),
            "就绪通知消费后应回到低频轮询，不能持续查询"
        );
        shutdown.cancel();
        task.await.expect("startup snapshot discard refreshes before periodic tick: fixture task and request must complete successfully");
        source.client.expect("startup snapshot discard refreshes before periodic tick: the fixture establishes source.client before this assertion").close();
    }

    /// [回归测试] 即使第一次快照已发布，随后建立会话也应唤醒快照任务。
    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_startup_session_after_first_snapshot_refreshes_immediately() {
        let (source, server, _directory) = setup().await;
        let shutdown = CancellationToken::new();
        let task = start(source.clone(), shutdown.clone());
        let first = request(&server, "peri/session_context").await;
        complete_initial_scan(&server, first, &source.cwd).await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(SERVICE_SNAPSHOT.state().read().cwd, source.cwd);
        new_session(&source, &server).await;
        complete_session_snapshot(&server, "ready-model").await;
        assert_eq!(SERVICE_SNAPSHOT.state().read().model_name, "ready-model");
        shutdown.cancel();
        task.await.expect("startup session after first snapshot refreshes immediately: fixture task and request must complete successfully");
        source.client.expect("startup session after first snapshot refreshes immediately: the fixture establishes source.client before this assertion").close();
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_startup_snapshot_shutdown_cancels_pending_query() {
        let (source, server, _directory) = setup().await;
        let shutdown = CancellationToken::new();
        let task = start(source.clone(), shutdown.clone());
        let _pending = request(&server, "peri/session_context").await;
        shutdown.cancel();
        tokio::time::timeout(Duration::from_millis(10), task)
            .await
            .expect("退出不得等待未返回的 RPC")
            .expect("startup snapshot shutdown cancels pending query: fixture task and request must complete successfully");
        source.client.expect("startup snapshot shutdown cancels pending query: the fixture establishes source.client before this assertion").close();
    }
}

use crate::kit::atoms::McpInitPhase;

use crate::kit::atoms::CRON_JOBS;

use crate::kit::atoms::CURRENT_SESSION_TITLE;

use crate::kit::atoms::FILE_LIST;

use crate::kit::atoms::HOOK_LIST;

use crate::kit::atoms::HookSummary;

use crate::kit::atoms::MCP_SERVERS;

use crate::kit::atoms::PLUGIN_LIST;

use crate::kit::atoms::SERVICE_PROJECTION_ERROR;

use crate::kit::atoms::SERVICE_SNAPSHOT;

use crate::kit::atoms::ServiceSnapshot;

use crate::kit::atoms::THREAD_LIST;

use crate::kit::atoms::ThreadBrowserScope;

use crate::kit::atoms::ThreadSummary;
