//! Tests for client

use super::*;
use crate::mcp::builtin::{
    context::{BuiltinContextError, BuiltinInstanceContext},
    runtime::BuiltinSpawnError,
};
use crate::mcp::oauth_flow::OAuthFailureKind;
use peri_acp_types::{
    dynamic_mcp::{DynamicMcpIncarnationId, DynamicMcpInstanceKey, DynamicMcpLogicalKey},
    plugin::McpSubscriptionsConfig,
};
use rmcp::model::SubscriptionFilter;

fn controlled_service(
    entered: tokio::sync::oneshot::Sender<()>,
    release: Arc<tokio::sync::Notify>,
    close_count: Arc<std::sync::atomic::AtomicUsize>,
) -> McpServiceWrapper {
    McpServiceWrapper::Controlled(ControlledMcpService::new(entered, release, close_count))
}

fn timing_out_service(close_count: Arc<std::sync::atomic::AtomicUsize>) -> McpServiceWrapper {
    McpServiceWrapper::Controlled(ControlledMcpService::timing_out(close_count))
}

fn connected_test_handle(name: &str) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
}

struct TestDropSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for TestDropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[test]
fn server_generation_advances_explicitly_on_each_committed_connection() {
    let pool = McpClientPool::new_pending();
    let first = connected_test_handle("server");
    let second = connected_test_handle("server");
    let service = || {
        let (entered, _entered_rx) = tokio::sync::oneshot::channel();
        controlled_service(
            entered,
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        )
    };

    assert!(pool
        .try_commit_connection("server".into(), Arc::clone(&first), service())
        .is_ok());
    let first_generation = pool.handle_generation(&first);
    assert!(pool
        .try_commit_connection("server".into(), Arc::clone(&second), service())
        .is_ok());
    let second_generation = pool.handle_generation(&second);

    assert!(first_generation > 0);
    assert!(second_generation > first_generation);
    assert_eq!(pool.handle_generation(&first), first_generation);
}

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
            version: None,
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
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
            version: None,
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
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

#[tokio::test]
async fn test_set_disabled_marks_handle_disabled() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.set_disabled("a").await;
    assert_eq!(
        pool.clients.read().get("a").map(|c| c.status.clone()),
        Some(ClientStatus::Disabled),
        "handle 应标记为 Disabled"
    );
}

#[test]
fn test_two_sessions_with_same_server_have_isolated_oauth_flows() {
    let pool = McpClientPool::new_pending();
    let connection = |session: &str, incarnation: &str| McpConnectionKey::Dynamic {
        instance: DynamicMcpInstanceKey {
            logical: DynamicMcpLogicalKey {
                session_id: session.to_string(),
                server_name: "docs".to_string(),
            },
            incarnation_id: DynamicMcpIncarnationId::from_string(incarnation),
        },
    };
    let first = connection("session-a", "mcpinc_a");
    let second = connection("session-b", "mcpinc_b");

    assert_eq!(
        pool.reserve_oauth_flow_scoped(first.clone(), "flow-a"),
        OAuthStartDisposition::Started
    );
    assert_eq!(
        pool.reserve_oauth_flow_scoped(second.clone(), "flow-b"),
        OAuthStartDisposition::Started
    );
    assert_eq!(
        pool.active_oauth_flow_scoped(&first).as_deref(),
        Some("flow-a")
    );
    assert_eq!(
        pool.active_oauth_flow_scoped(&second).as_deref(),
        Some("flow-b")
    );
    assert_eq!(first.server_name(), "docs");
    assert!(first.is_dynamic());
    assert!(!pool.persistent_cache_allowed_for(&first));
    assert!(!pool.persistent_cache_allowed_for(&second));
}

#[test]
fn test_dynamic_oauth_callback_requires_full_instance_identity_and_rejects_late_l1() {
    let pool = McpClientPool::new_pending();
    let connection = |incarnation: &str| McpConnectionKey::Dynamic {
        instance: DynamicMcpInstanceKey {
            logical: DynamicMcpLogicalKey {
                session_id: "session-a".to_string(),
                server_name: "docs".to_string(),
            },
            incarnation_id: DynamicMcpIncarnationId::from_string(incarnation),
        },
    };
    let l1 = connection("mcpinc_l1");
    let l2 = connection("mcpinc_l2");
    assert_eq!(
        pool.reserve_oauth_flow_scoped(l1.clone(), "flow-l1"),
        OAuthStartDisposition::Started
    );
    let (l1_tx, _l1_rx) = tokio::sync::oneshot::channel();
    assert!(pool.register_oauth_callback_scoped(l1.clone(), "flow-l1", l1_tx));
    pool.revoke_oauth_connection(&l1);

    assert_eq!(
        pool.reserve_oauth_flow_scoped(l2.clone(), "flow-l2"),
        OAuthStartDisposition::Started
    );
    let (l2_tx, _l2_rx) = tokio::sync::oneshot::channel();
    assert!(pool.register_oauth_callback_scoped(l2.clone(), "flow-l2", l2_tx));
    assert!(pool
        .deliver_oauth_callback_scoped(
            &l1,
            "flow-l1",
            "late-code".to_string(),
            "late-state".to_string(),
        )
        .is_err());
    assert_eq!(
        pool.active_oauth_flow_scoped(&l2).as_deref(),
        Some("flow-l2")
    );
    assert!(!pool.cancel_oauth_flow_scoped(&l1, "flow-l1"));
    assert!(pool.cancel_oauth_flow_scoped(&l2, "flow-l2"));
}

#[test]
fn test_oauth_flow_reservation_is_idempotent_and_rejects_competing_identity() {
    let pool = McpClientPool::new_pending();
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow-1"),
        OAuthStartDisposition::Started
    );
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow-1"),
        OAuthStartDisposition::AlreadyActive
    );
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow-2"),
        OAuthStartDisposition::Conflict {
            active_flow_id: "flow-1".to_string()
        }
    );
    assert_eq!(pool.active_oauth_flow("docs").as_deref(), Some("flow-1"));
}

#[test]
fn test_oauth_late_release_cannot_clear_newer_flow() {
    let pool = McpClientPool::new_pending();
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow-new"),
        OAuthStartDisposition::Started
    );
    pool.release_oauth_flow("docs", "flow-old");
    assert_eq!(
        pool.active_oauth_flow("docs").as_deref(),
        Some("flow-new"),
        "晚到旧终态不得释放当前 flow"
    );
}

#[test]
fn test_oauth_cancel_requires_exact_flow_identity() {
    let pool = McpClientPool::new_pending();
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow-1"),
        OAuthStartDisposition::Started
    );
    let (tx, _rx) = tokio::sync::oneshot::channel();
    assert!(pool.register_oauth_callback("docs", "flow-1", tx));
    assert!(!pool.cancel_oauth_flow("flow-other"));
    assert_eq!(pool.active_oauth_flow("docs").as_deref(), Some("flow-1"));
    assert!(pool.cancel_oauth_flow("flow-1"));
    assert_eq!(
        pool.active_oauth_flow("docs").as_deref(),
        Some("flow-1"),
        "取消请求只关闭 callback，须等精确终态再释放 reservation"
    );
    pool.release_oauth_flow("docs", "flow-1");
    assert!(pool.active_oauth_flow("docs").is_none());
}

#[tokio::test]
async fn test_oauth_preflight_failure_emits_one_exact_terminal_event() {
    let pool = Arc::new(McpClientPool::new_pending());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_clone = observed.clone();
    pool.set_oauth_event_callback(move |event| {
        if let OAuthFlowEvent::AuthorizationFailed {
            flow_id,
            server_name,
            failure_kind,
            ..
        } = event
        {
            observed_clone
                .lock()
                .unwrap()
                .push((flow_id, server_name, failure_kind));
        }
    });
    let result = pool.start_oauth_flow("flow-1", "missing", false).await;
    assert!(matches!(result, Err(McpPoolError::NotConnected { .. })));
    assert_eq!(
        *observed.lock().unwrap(),
        vec![(
            "flow-1".to_string(),
            "missing".to_string(),
            OAuthFailureKind::Internal
        )]
    );
}

#[test]
fn test_persistent_cache_is_disabled_for_authenticated_servers() {
    let config = || McpServerConfig {
        task_scope_secret_file: None,
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        subscriptions: None,
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        source: None,
    };
    let pool = McpClientPool::new_empty();
    pool.configs.write().insert(
        "oauth".to_string(),
        McpServerConfig {
            oauth: Some(Default::default()),
            ..config()
        },
    );
    pool.configs.write().insert(
        "header".to_string(),
        McpServerConfig {
            headers: Some(std::collections::HashMap::from([(
                "X-Test-Header".to_string(),
                "present".to_string(),
            )])),
            ..config()
        },
    );

    pool.configs.write().insert(
        "custom-header".to_string(),
        McpServerConfig {
            headers: Some(std::collections::HashMap::from([(
                "X-Client-Label".to_string(),
                "test".to_string(),
            )])),
            ..config()
        },
    );

    pool.configs.write().insert(
        "query".to_string(),
        McpServerConfig {
            url: Some("https://example.test/mcp?mode=test".to_string()),
            ..config()
        },
    );
    pool.configs.write().insert(
        "env".to_string(),
        McpServerConfig {
            env: Some(std::collections::HashMap::from([(
                "MCP_TEST_MODE".to_string(),
                "enabled".to_string(),
            )])),
            ..config()
        },
    );

    assert!(!pool.persistent_cache_allowed("oauth"));
    assert!(!pool.persistent_cache_allowed("header"));
    assert!(
        !pool.persistent_cache_allowed("custom-header"),
        "未知静态 header 可能是服务自定义凭据，必须保守禁用持久化 cache"
    );
    assert!(
        !pool.persistent_cache_allowed("query"),
        "URL query 可能携带访问令牌，必须保守禁用持久化 cache"
    );
    assert!(
        !pool.persistent_cache_allowed("env"),
        "stdio env 可能携带凭据，必须保守禁用持久化 cache"
    );
    assert!(pool.persistent_cache_allowed("unknown"));
}

#[test]
fn test_tools_cache_eligible_requires_version_and_allowed_policy() {
    let config = || McpServerConfig {
        task_scope_secret_file: None,
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        subscriptions: None,
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        source: None,
    };
    let pool = McpClientPool::new_empty();
    pool.configs.write().insert(
        "oauth".to_string(),
        McpServerConfig {
            oauth: Some(Default::default()),
            ..config()
        },
    );
    pool.configs.write().insert("plain".to_string(), config());

    // 无版本：即便安全策略允许也禁止跨进程复用（对应「无版本不命中」）。
    assert!(
        !pool.tools_cache_eligible("plain"),
        "server 未声明 cache-version 时不得复用磁盘 tools/list"
    );
    // 安全策略拒绝持久化：即便已声明版本也禁止复用（对应「安全策略回退」）。
    assert!(
        !pool.tools_cache_eligible("oauth"),
        "OAuth server 必须保持原始网络行为，不得读盘"
    );
    // 声明版本且策略允许：可跨进程复用（对应「版本命中」准入）。
    pool.cache_versions
        .write()
        .insert("plain".to_string(), "opaque-v1".to_string());
    assert!(
        pool.tools_cache_eligible("plain"),
        "声明 version 且策略允许时应复用磁盘 tools/list"
    );
}

#[test]
fn test_cache_scope_persistence_accepts_known_scopes_only() {
    assert!(cache_scope_allows_persistence(Some(
        rmcp::model::CacheScope::Public
    )));
    assert!(cache_scope_allows_persistence(Some(
        rmcp::model::CacheScope::Private
    )));
    assert!(!cache_scope_allows_persistence(None));
}

#[test]
fn test_server_info_projects_safe_failed_status() {
    let status = ClientStatus::Failed(
        "request failed: https://example.test/mcp?token=top-secret\ncaused by: verbose trace"
            .to_string(),
    );

    assert_eq!(mcp_status_label(&status), "failed");
    let summary = mcp_error_summary(&status).unwrap();
    assert_eq!(summary, "request failed: https://example.test/mcp?…");
    assert!(!summary.contains("top-secret"));
    assert!(!summary.contains("verbose trace"));
}

#[test]
fn test_redact_mcp_error_masks_secret_assignment() {
    assert_eq!(
        redact_mcp_error("connection failed token=top-secret"),
        "connection failed [redacted]"
    );
}

#[path = "client_builtin_context_test.rs"]
mod builtin_context_tests;

#[path = "client_shutdown_test.rs"]
mod shutdown_tests;
