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
