//! Tests for service_snapshot

#[cfg(test)]
use super::*;
use crate::app::service_registry::ProcessResourceMonitor;
use chrono::Utc;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
use peri_middlewares::cron::CronScheduler;
use serde_json::{Value, json};
use serial_test::serial;

fn reset_snapshot_atoms() {
    crate::kit::atoms::init_atoms();
    ACTIVE_EXECUTION_CWD.set(None);
    ACTIVE_SESSION_ID.set(String::new());
    THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Project);
    THREAD_LIST_PAGE_COUNT.set(1);
    THREAD_LIST_ERROR.set(None);
    THREAD_LIST.state().write().clear();
}

fn isolated_refresh() -> SlowSnapshotRefresh {
    SlowSnapshotRefresh {
        next_memory_scan: Instant::now() + Duration::from_secs(3600),
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
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
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
                "session/list" => {
                    queries.send(params).unwrap();
                    json!({"sessions":[],"_meta":{"peri.sessionWorkspaceV1":{
                        "threads":[entry_json(&cwd,"Project thread")],"nextCursor":null}}})
                }
                other => panic!("unexpected request: {other}"),
            };
            server.send_response(id, Ok(result)).await.unwrap();
        }
    });
    client.register_ui_commands(&[]).await.unwrap();
    (client, rx)
}

#[tokio::test]
#[serial]
async fn project_history_uses_host_scope_and_actual_execution_path() {
    reset_snapshot_atoms();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let (client, mut queries) = snapshot_client(cwd).await;
    let mut src = make_minimal_source(Some(client));
    src.cwd = cwd.into();
    let mut slow = isolated_refresh();
    tick_once(&src, &mut slow).await.unwrap();
    assert_eq!(
        queries.recv().await.unwrap()["_meta"]["peri.sessionWorkspaceV1"]["scope"]["kind"],
        "project"
    );
    let threads = THREAD_LIST.state().read().clone();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].cwd, cwd);
    assert_eq!(threads[0].message_count, 3);
    THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Workspace);
    tick_once(&src, &mut slow).await.unwrap();
    assert_eq!(
        queries.recv().await.unwrap()["_meta"]["peri.sessionWorkspaceV1"]["scope"]["kind"],
        "workspace"
    );
}

#[tokio::test]
#[serial]
async fn active_execution_directory_invalidates_file_cache_and_updates_title() {
    reset_snapshot_atoms();
    let original = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    std::fs::write(original.path().join("original.rs"), "old").unwrap();
    std::fs::write(target.path().join("target.rs"), "new").unwrap();
    let (client, _queries) = snapshot_client(target.path().to_str().unwrap()).await;
    let mut src = make_minimal_source(Some(client));
    src.cwd = original.path().to_str().unwrap().into();
    let mut slow = isolated_refresh();
    tick_once(&src, &mut slow).await.unwrap();
    assert_eq!(*FILE_LIST.state().read(), vec!["original.rs"]);
    ACTIVE_EXECUTION_CWD.set(Some(target.path().to_str().unwrap().into()));
    ACTIVE_SESSION_ID.set("00000000-0000-0000-0000-000000000003".into());
    tick_once(&src, &mut slow).await.unwrap();
    assert_eq!(*FILE_LIST.state().read(), vec!["target.rs"]);
    assert_eq!(
        SERVICE_SNAPSHOT.state().read().cwd,
        target.path().to_str().unwrap()
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
async fn missing_workspace_capability_is_visible_as_query_error() {
    reset_snapshot_atoms();
    let (transport, _server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    let src = make_minimal_source(Some(client));
    tick_once(&src, &mut isolated_refresh()).await.unwrap();
    assert!(
        THREAD_LIST_ERROR
            .state()
            .read()
            .clone()
            .unwrap()
            .contains("does not support")
    );
    assert!(THREAD_LIST.state().read().is_empty());
}

fn make_minimal_source(client: Option<AcpTuiClient>) -> SnapshotSource {
    let peri_config = Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    ));
    let permission_mode = SharedPermissionMode::new(PermissionMode::Default);
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(
        tokio::sync::mpsc::unbounded_channel().0,
    )));
    let monitor = Arc::new(Mutex::new(ProcessResourceMonitor::new()));

    SnapshotSource {
        cwd: ".".into(),
        client,
        peri_config,
        permission_mode,
        cron_scheduler: scheduler,
        mcp_pool: None,
        mcp_init_rx: None,
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
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok(), "tick_once should succeed");

    let snap = SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cwd, ".");
    assert_eq!(snap.cron_total, 0);
    assert_eq!(snap.cron_enabled, 0);
    assert_eq!(snap.mcp.total, 0);
    assert_eq!(snap.mcp.connected, 0);
    assert_eq!(snap.mcp.init_phase, McpInitPhase::Pending);
}

#[tokio::test]
#[serial]
async fn test_tick_once_empty_thread_list() {
    reset_snapshot_atoms();

    let src = make_minimal_source(None);
    // ACP 项目列表为空时不生成会话行
    let mut slow = isolated_refresh();
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok());

    let threads = THREAD_LIST.state().read().clone();
    assert!(threads.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cron_tasks_collected() {
    reset_snapshot_atoms();

    let src = make_minimal_source(None);
    // 注册两个 cron 任务（一个 disabled）
    {
        let mut scheduler = src.cron_scheduler.lock();
        let _ = scheduler.register("*/5 * * * *", "test prompt 1").unwrap();
        let id2 = scheduler.register("*/10 * * * *", "test prompt 2").unwrap();
        scheduler.toggle(&id2); // disable
    }

    let mut slow = isolated_refresh();
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok());

    let jobs = CRON_JOBS.state().read().clone();
    assert_eq!(jobs.len(), 2);

    let snap = SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cron_total, 2);
    assert_eq!(snap.cron_enabled, 1);
}

#[tokio::test]
async fn test_derive_provider_and_model_default() {
    let peri_config = Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    ));
    let (provider, alias, model_name, effort) = derive_provider_and_model(&peri_config);
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
                profiles.get_mut("sonnet").unwrap().provider = "p1".into();
                profiles.get_mut("sonnet").unwrap().effort = "high".into();
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
    let (provider, alias, model_name, effort) = derive_provider_and_model(&peri_config);
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
                profiles.get_mut("haiku").unwrap().provider = "p1".into();
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
    let (provider, alias, model_name, effort) = derive_provider_and_model(&peri_config);
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
                profiles.get_mut("haiku").unwrap().provider = "p1".into();
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
    let (provider, alias, model_name, effort) = derive_provider_and_model(&peri_config);
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
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // 顶层文件
    std::fs::write(root.join("a.txt"), "x").unwrap();
    std::fs::write(root.join("b.rs"), "x").unwrap();
    // 子目录文件（深度=2）
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("mod.rs"), "x").unwrap();
    // 忽略目录：node_modules 内文件不应出现
    std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
    std::fs::write(
        root.join("node_modules").join("pkg").join("ignored.js"),
        "x",
    )
    .unwrap();

    let files = scan_cwd_files_shallow(root.to_str().unwrap());
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
    let files = scan_cwd_files_shallow("/this/path/does/not/exist");
    assert!(files.is_empty());
}

/// C2 回归测试：MAX_FILES 上限防止无限增长。
#[test]
fn test_scan_cwd_files_shallow_caps_at_max() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // 创建 600 个文件（超过 MAX_FILES=500）
    for i in 0..600 {
        std::fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
    }
    let files = scan_cwd_files_shallow(root.to_str().unwrap());
    assert!(files.len() <= 500, "should cap at 500, got {}", files.len());
}
