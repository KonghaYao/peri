//! Tests for service_snapshot

#[cfg(test)]
use super::*;
use crate::app::service_registry::ProcessResourceMonitor;
use chrono::Utc;
use peri_middlewares::cron::CronScheduler;
use peri_resources::sessions::SqliteThreadStore;
use serial_test::serial;

/// 创建一个 SQLite in-memory thread store 用于测试。
async fn make_sqlite_store() -> Arc<dyn ThreadStore> {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("test_threads.db");
    // SAFETY: tempdir 保持到函数返回前有效；我们 leak 它让 store 在测试期间存活。
    // 测试结束后 OS 会清理 tempfile。
    std::mem::forget(tmp);
    Arc::new(SqliteThreadStore::new(path).await.unwrap())
}

fn make_minimal_source(thread_store: Arc<dyn ThreadStore>) -> SnapshotSource {
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
        thread_store,
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
    crate::kit::atoms::init_atoms();

    let store = make_sqlite_store().await;
    let src = make_minimal_source(store);
    let mut slow = SlowSnapshotRefresh::default();
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
    crate::kit::atoms::init_atoms();

    let store = make_sqlite_store().await;
    let src = make_minimal_source(store);
    // 空 SQLite store——list_threads 返回空 Vec
    let mut slow = SlowSnapshotRefresh::default();
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok());

    let threads = THREAD_LIST.state().read().clone();
    assert!(threads.is_empty());
}

#[tokio::test]
#[serial]
async fn test_cron_tasks_collected() {
    crate::kit::atoms::init_atoms();

    let store = make_sqlite_store().await;
    let src = make_minimal_source(store);
    // 注册两个 cron 任务（一个 disabled）
    {
        let mut scheduler = src.cron_scheduler.lock();
        let _ = scheduler.register("*/5 * * * *", "test prompt 1").unwrap();
        let id2 = scheduler.register("*/10 * * * *", "test prompt 2").unwrap();
        scheduler.toggle(&id2); // disable
    }

    let mut slow = SlowSnapshotRefresh::default();
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok());

    let jobs = CRON_JOBS.state().read().clone();
    assert_eq!(jobs.len(), 2);

    let snap = SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cron_total, 2);
    assert_eq!(snap.cron_enabled, 1);
}

#[tokio::test]
#[serial]
async fn test_tick_once_derives_current_session_title() {
    use peri_acp_types::thread::ThreadMeta;

    crate::kit::atoms::init_atoms();
    crate::kit::atoms::ACTIVE_SESSION_ID.set(String::new());

    let store = make_sqlite_store().await;
    let src = make_minimal_source(store.clone());

    // 建一个带标题的 thread，并设为当前会话
    let mut meta = ThreadMeta::new(".".to_string());
    meta.title = Some("测试会话".to_string());
    let id = src.thread_store.create_thread(meta).await.unwrap();
    crate::kit::atoms::ACTIVE_SESSION_ID.set(id.clone());

    let mut slow = SlowSnapshotRefresh::default();
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok());

    assert_eq!(
        crate::kit::atoms::CURRENT_SESSION_TITLE
            .state()
            .read()
            .as_str(),
        "测试会话"
    );

    // 空标题 thread：tick 后应清空 CURRENT_SESSION_TITLE（会话切到无标题 thread）
    let meta2 = ThreadMeta::new(".".to_string());
    let id2 = src.thread_store.create_thread(meta2).await.unwrap();
    crate::kit::atoms::ACTIVE_SESSION_ID.set(id2);
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok());
    assert!(
        crate::kit::atoms::CURRENT_SESSION_TITLE
            .state()
            .read()
            .is_empty()
    );
}

#[tokio::test]
#[serial]
async fn test_tick_once_missing_thread_keeps_previous_title() {
    use peri_acp_types::thread::ThreadMeta;

    crate::kit::atoms::init_atoms();

    let store = make_sqlite_store().await;
    let src = make_minimal_source(store.clone());

    // 先派生一个真实标题
    let mut meta = ThreadMeta::new(".".to_string());
    meta.title = Some("真实标题".to_string());
    let id = src.thread_store.create_thread(meta).await.unwrap();
    crate::kit::atoms::ACTIVE_SESSION_ID.set(id.clone());
    let mut slow = SlowSnapshotRefresh::default();
    let _ = tick_once(&src, &mut slow).await;
    assert_eq!(
        crate::kit::atoms::CURRENT_SESSION_TITLE
            .state()
            .read()
            .as_str(),
        "真实标题"
    );

    // 切到不存在的 thread id：load_meta 失败 → 保留上一个标题（不 panic）
    crate::kit::atoms::ACTIVE_SESSION_ID.set("nonexistent-id".to_string());
    let result = tick_once(&src, &mut slow).await;
    assert!(result.is_ok());
    assert_eq!(
        crate::kit::atoms::CURRENT_SESSION_TITLE
            .state()
            .read()
            .as_str(),
        "真实标题"
    );
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
