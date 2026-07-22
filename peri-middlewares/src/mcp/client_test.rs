//! Tests for client

use super::*;

#[test]
fn test_pool_get_all_clients_filters_disconnected() {
    let pool = McpClientPool::new_empty();
    assert!(pool.get_all_clients().is_empty());
}
#[test]
fn test_pool_has_no_resources() {
    assert!(!McpClientPool::new_empty().has_resources());
}
#[test]
fn test_resource_summary_empty() {
    assert!(McpClientPool::new_empty().resource_summary().is_empty());
}
#[test]
fn test_client_status_equality() {
    assert_eq!(ClientStatus::Connected, ClientStatus::Connected);
    assert_ne!(
        ClientStatus::Failed("a".into()),
        ClientStatus::Failed("b".into())
    );
}
#[test]
fn test_mcp_init_status_equality() {
    assert_eq!(McpInitStatus::Pending, McpInitStatus::Pending);
    assert_eq!(
        McpInitStatus::Initializing {
            connected: 1,
            total: 2
        },
        McpInitStatus::Initializing {
            connected: 1,
            total: 2
        }
    );
    assert_ne!(
        McpInitStatus::Ready { total: 3 },
        McpInitStatus::Ready { total: 4 }
    );
}
#[test]
fn test_new_pending_creates_empty_pool() {
    let pool = McpClientPool::new_pending();
    assert!(pool.clients.read().is_empty());
}
#[test]
fn test_server_infos_empty_pool() {
    assert!(McpClientPool::new_pending().server_infos().is_empty());
}
#[tokio::test]
async fn test_insert_failed() {
    let pool = Arc::new(McpClientPool::new_pending());
    McpClientPool::insert_failed(&pool, "s", "err".into());
    assert_eq!(
        pool.server_infos()[0].status,
        ClientStatus::Failed("err".into())
    );
}
#[tokio::test]
async fn test_remove_server() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.clients.write().insert(
        "a".into(),
        Arc::new(McpClientHandle {
            name: "a".into(),
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            channel_capable: false,
        }),
    );
    pool.remove_server("a").await;
    assert!(pool.server_infos().is_empty());
}
#[tokio::test]
async fn test_get_tools_resources() {
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "s".into(),
        Arc::new(McpClientHandle {
            name: "s".into(),
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            channel_capable: false,
        }),
    );
    assert!(pool.get_tools("s").is_empty());
    assert!(pool.get_tools("x").is_empty());
}

#[test]
fn test_plugin_source_of_empty_pool_returns_none() {
    let pool = McpClientPool::new_pending();
    assert!(pool.plugin_source_of("any").is_none());
}

#[test]
fn test_plugin_source_of_after_write_returns_value() {
    let pool = McpClientPool::new_pending();
    pool.plugin_sources
        .write()
        .insert("p1__srv1".to_string(), "p1@marketplace_a".to_string());
    assert_eq!(
        pool.plugin_source_of("p1__srv1"),
        Some("p1@marketplace_a".to_string())
    );
}

#[test]
fn test_plugin_source_of_nonexistent_returns_none() {
    let pool = McpClientPool::new_pending();
    pool.plugin_sources
        .write()
        .insert("p1__srv1".to_string(), "p1@alpha".to_string());
    assert!(pool.plugin_source_of("nonexistent").is_none());
}
