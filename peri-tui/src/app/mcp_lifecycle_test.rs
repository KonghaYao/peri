#[tokio::test]
async fn test_tui_panel_pool_retains_owner_and_uses_ordered_shutdown() {
    let (owner, spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    let pool = std::sync::Arc::new(
        peri_middlewares::mcp::McpClientPool::new_pending_with_spawner(spawner),
    );
    let weak = std::sync::Arc::downgrade(&pool);
    let task_pool = pool.clone();
    pool.spawn_background(peri_middlewares::mcp::McpTaskKey::Initialize, async move {
        let _pool = task_pool;
        std::future::pending::<()>().await;
    })
    .unwrap();

    crate::launch::shutdown_mcp_pool(pool, Some(owner)).await;

    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn test_tui_reconnect_racing_begin_shutdown_is_registered_or_rejected() {
    let (mut winning_owner, winning_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    let winning_pool = std::sync::Arc::new(
        peri_middlewares::mcp::McpClientPool::new_pending_with_spawner(winning_spawner),
    );
    assert!(winning_pool.spawn_reconnect("server".into()).is_ok());
    winning_pool.begin_shutdown();
    winning_owner.begin_shutdown();
    winning_owner.shutdown().await;
    winning_pool.shutdown().await;

    let (mut losing_owner, losing_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    let losing_pool = std::sync::Arc::new(
        peri_middlewares::mcp::McpClientPool::new_pending_with_spawner(losing_spawner),
    );
    losing_pool.begin_shutdown();
    assert!(losing_pool.spawn_reconnect("server".into()).is_err());
    losing_owner.begin_shutdown();
    losing_owner.shutdown().await;
    losing_pool.shutdown().await;
}
