use super::*;

#[tokio::test]
async fn configuration_policy_is_installed_before_empty_pool_becomes_ready() {
    for setting in [None, Some(true), Some(false)] {
        let pool = Arc::new(McpClientPool::new_pending());
        let (status, received) = tokio::sync::watch::channel(McpInitStatus::Pending);
        let directory = tempfile::tempdir().unwrap();
        let config = super::super::config::McpConfigFile {
            mcp_cache: setting,
            ..Default::default()
        };
        McpClientPool::initialize_config(
            pool.clone(),
            directory.path(),
            config,
            Default::default(),
            status,
            None,
        )
        .await;
        assert_eq!(*received.borrow(), McpInitStatus::Ready { total: 0 });
        assert_eq!(
            pool.persistent_cache_allowed("any-server"),
            setting != Some(false)
        );
        let policy = super::super::config::McpCachePolicy::from_setting(setting);
        pool.bind_cache_policy(policy).unwrap();
        let opposite = if policy == super::super::config::McpCachePolicy::Disabled {
            super::super::config::McpCachePolicy::Enabled
        } else {
            super::super::config::McpCachePolicy::Disabled
        };
        assert!(pool.bind_cache_policy(opposite).is_err());
    }
}

#[tokio::test]
async fn reinitialization_cannot_reenable_disabled_cache_policy() {
    let pool = Arc::new(McpClientPool::new_pending());
    let directory = tempfile::tempdir().unwrap();
    for setting in [false, true] {
        let (status, received) = tokio::sync::watch::channel(McpInitStatus::Pending);
        let config = super::super::config::McpConfigFile {
            mcp_cache: Some(setting),
            ..Default::default()
        };
        McpClientPool::initialize_config(
            pool.clone(),
            directory.path(),
            config,
            Default::default(),
            status,
            None,
        )
        .await;
        if setting {
            assert!(matches!(*received.borrow(), McpInitStatus::Failed(_)));
        } else {
            assert_eq!(*received.borrow(), McpInitStatus::Ready { total: 0 });
        }
        assert!(!pool.persistent_cache_allowed("any-server"));
    }
}

fn frozen_snapshot(directory: &Path, file_cache: bool) -> Arc<peri_config::ConfigurationSnapshot> {
    let scope =
        peri_config::ConfigurationScope::new(directory.to_owned(), directory.join("settings.json"))
            .unwrap();
    Arc::new(
        peri_config::ConfigurationSnapshot::resolve(
            scope,
            peri_config::ConfigurationInputs {
                global: Some(format!(r#"{{"mcpCache":{file_cache}}}"#)),
                environment: std::collections::BTreeMap::from([
                    ("PERI_MCP_BUILTIN".into(), "off".into()),
                    ("PERI_MCP_CACHE".into(), "on".into()),
                ]),
                ..Default::default()
            },
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn bound_configuration_snapshot_freezes_cache_and_files_until_new_pool() {
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    let snapshot = frozen_snapshot(directory.path(), false);
    let revision = snapshot.revision();
    pool.set_configuration_snapshot(snapshot).unwrap();
    assert_eq!(pool.configuration_revision(), Some(revision));
    std::fs::write(
        directory.path().join("settings.json"),
        "invalid changed input",
    )
    .unwrap();
    std::fs::write(directory.path().join(".mcp.json"), "invalid changed input").unwrap();
    let (status, received) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize(
        pool.clone(),
        directory.path(),
        &directory.path().join("no-plugins"),
        status,
        None,
    )
    .await;
    assert_eq!(*received.borrow(), McpInitStatus::Ready { total: 0 });
    assert!(!pool.persistent_cache_allowed("any-server"));
    assert_eq!(pool.configuration_revision(), Some(revision));
    assert!(pool
        .set_configuration_snapshot(frozen_snapshot(directory.path(), true))
        .is_err());
}

#[tokio::test]
async fn bare_configuration_snapshot_ignores_file_cache_restrictions() {
    let directory = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.set_configuration_snapshot(frozen_snapshot(directory.path(), false))
        .unwrap();
    let (status, received) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize_bare(pool.clone(), directory.path(), status).await;
    assert_eq!(*received.borrow(), McpInitStatus::Ready { total: 0 });
    assert!(pool.persistent_cache_allowed("any-server"));
}

#[tokio::test]
async fn bound_configuration_scope_cannot_be_reused_for_another_project() {
    let directory = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.set_configuration_snapshot(frozen_snapshot(directory.path(), true))
        .unwrap();
    let (status, received) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize(
        pool.clone(),
        other.path(),
        &other.path().join("no-plugins"),
        status,
        None,
    )
    .await;
    assert!(matches!(*received.borrow(), McpInitStatus::Failed(_)));
    assert!(!pool.initialized.load(std::sync::atomic::Ordering::SeqCst));
    assert!(pool.configs.read().is_empty());
}
