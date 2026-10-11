//! host 装配面目标测试：池的 cwd 绑定必须发生在延迟初始化之前（STD-SIZE-001 拆分，
//! 原 `assemble.rs` 内联 `mod tests` 迁出）。

use super::*;

#[tokio::test]
async fn worktree_mcp_pool_is_bound_before_deferred_initialization() {
    let target = tempfile::TempDir::new().unwrap();
    let sibling = tempfile::TempDir::new().unwrap();
    let (resources, shutdown) =
        peri_resources::Resources::open_with(Some(target.path().join("configured.db")))
            .await
            .unwrap()
            .into_parts();
    let (_owner, spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    let pool = pending_mcp_pool(
        spawner,
        peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        Some(target.path()),
        None,
        &resources,
    );
    assert_eq!(pool.snapshot()["initPhase"], "pending");
    assert!(pool.bind_execution_cwd(sibling.path()).is_err());
    assert_eq!(
        pool.bind_execution_cwd(target.path()).unwrap(),
        target.path()
    );
    peri_acp_types::session_resources::SessionStoreShutdownPort::shutdown(&shutdown)
        .await
        .unwrap();
}
