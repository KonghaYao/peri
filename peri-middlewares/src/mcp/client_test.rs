//! Tests for client

use super::*;
use peri_acp_types::plugin::McpSubscriptionsConfig;
use rmcp::model::SubscriptionFilter;

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

// ── build_subscription_filter（2026-07-28 subscriptions/listen）──────────────

/// build_subscription_filter：四种过滤器与 McpSubscriptionsConfig 字段一一映射。
#[test]
fn test_build_subscription_filter_maps_all_fields() {
    let sub = McpSubscriptionsConfig {
        resources: vec![
            "file:///notes/1.md".to_string(),
            "file:///notes/2.md".to_string(),
        ],
        tools_list_changed: true,
        prompts_list_changed: true,
        resources_list_changed: true,
    };
    let filter = build_subscription_filter(&sub);
    // SubscriptionFilter 为 #[non_exhaustive]，期望值经 builder 构造
    let expected = SubscriptionFilter::builder()
        .resource_subscriptions(vec![
            "file:///notes/1.md".to_string(),
            "file:///notes/2.md".to_string(),
        ])
        .tools_list_changed()
        .prompts_list_changed()
        .resources_list_changed()
        .build();
    assert_eq!(
        filter, expected,
        "字段映射必须与 rmcp SubscriptionFilter 语义一致"
    );
}

/// build_subscription_filter：全 None（空配置）→ 空过滤器（不订阅任何通知）。
#[test]
fn test_build_subscription_filter_empty_config_yields_empty_filter() {
    let filter = build_subscription_filter(&McpSubscriptionsConfig::default());
    assert_eq!(filter, SubscriptionFilter::new(), "空配置不得订阅任何通知");
    assert!(filter.tools_list_changed.is_none());
    assert!(filter.prompts_list_changed.is_none());
    assert!(filter.resources_list_changed.is_none());
    assert!(filter.resource_subscriptions.is_none());
}

/// build_subscription_filter：仅配置 resources 时，其余布尔字段保持 None。
#[test]
fn test_build_subscription_filter_resources_only_keeps_others_none() {
    let sub = McpSubscriptionsConfig {
        resources: vec!["file:///only-resource.md".to_string()],
        ..Default::default()
    };
    let filter = build_subscription_filter(&sub);
    assert_eq!(filter.tools_list_changed, None);
    assert_eq!(filter.prompts_list_changed, None);
    assert_eq!(filter.resources_list_changed, None);
    assert_eq!(
        filter.resource_subscriptions.as_deref(),
        Some(&["file:///only-resource.md".to_string()][..]),
        "resources 应原样映射到 resource_subscriptions"
    );
}

// ── remove_server / set_disabled 的 subscription_tasks 清理 ─────────────────

/// remove_server 须清理 subscription_tasks 条目（订阅循环任务随之终止，
/// 防止残留任务继续广播通知）。
#[tokio::test]
async fn test_remove_server_clears_subscription_tasks() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.subscription_tasks
        .lock()
        .await
        .insert("a".to_string(), vec![tokio::spawn(async {})]);
    pool.remove_server("a").await;
    assert!(
        pool.subscription_tasks.lock().await.is_empty(),
        "remove_server 后 subscription_tasks 必须清空"
    );
}

/// set_disabled 须清理 subscription_tasks 条目（禁用后订阅循环不得残留）。
#[tokio::test]
async fn test_set_disabled_clears_subscription_tasks() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.subscription_tasks
        .lock()
        .await
        .insert("a".to_string(), vec![tokio::spawn(async {})]);
    pool.set_disabled("a").await;
    assert!(
        pool.subscription_tasks.lock().await.is_empty(),
        "set_disabled 后 subscription_tasks 必须清空"
    );
    assert_eq!(
        pool.clients.read().get("a").map(|c| c.status.clone()),
        Some(ClientStatus::Disabled),
        "handle 应标记为 Disabled"
    );
}
