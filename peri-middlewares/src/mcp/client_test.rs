//! Tests for client

use super::*;
use crate::mcp::oauth_flow::OAuthFailureKind;
use peri_acp_types::plugin::McpSubscriptionsConfig;
use rmcp::model::SubscriptionFilter;
use std::time::Duration;

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
        channel_capable: false,
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
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
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
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
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

#[tokio::test]
async fn test_invalidate_tools_cache_clears_disk_entry() {
    let pool = McpClientPool::new_empty();
    let origin = pool.cache_origin("tool-server");
    let cache = pool.resource_cache();
    let ticket = cache.ticket(&origin, "tools/list", "").await.unwrap();
    cache
        .put_ticket_versioned(
            &ticket,
            Duration::from_secs(60),
            Some("opaque-v1"),
            &vec![rmcp::model::Tool::default()],
        )
        .await;

    let before: Option<Vec<rmcp::model::Tool>> = cache
        .get_versioned(&origin, "tools/list", "", Some("opaque-v1"))
        .await;
    assert!(before.is_some(), "前置：失效前命中");

    // 这是 subscriptions/listen 收到 `notifications/tools/list_changed` 后调用的路径。
    pool.invalidate_tools_cache("tool-server").await;

    let after: Option<Vec<rmcp::model::Tool>> = cache
        .get_versioned(&origin, "tools/list", "", Some("opaque-v1"))
        .await;
    assert!(
        after.is_none(),
        "tools/list_changed 必须使磁盘 tools/list 缓存失效"
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

#[tokio::test]
async fn test_shutdown_drops_pending_oauth_callbacks() {
    let pool = McpClientPool::new_empty();
    assert_eq!(
        pool.reserve_oauth_flow("server", "flow"),
        OAuthStartDisposition::Started
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    assert!(pool.register_oauth_callback("server", "flow", tx));

    pool.shutdown().await;

    assert!(
        rx.await.is_err(),
        "shutdown must cancel the callback waiter"
    );
    assert!(pool.active_oauth_flow("server").is_none());
}

#[tokio::test]
async fn test_shutdown_is_idempotent_and_closes_admission() {
    let pool = McpClientPool::new_empty();
    pool.shutdown().await;
    pool.shutdown().await;

    let (tx, _rx) = tokio::sync::oneshot::channel();
    assert!(!pool.register_oauth_callback("server", "flow", tx));
    assert!(!pool.is_open());
    assert!(pool.services.lock().is_empty());
}

#[tokio::test]
async fn test_cancelled_shutdown_waiter_keeps_single_service_transaction_owned() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = Arc::new(McpClientPool::new_empty());
    let first_release = Arc::new(tokio::sync::Notify::new());
    let second_release = Arc::new(tokio::sync::Notify::new());
    let first_count = Arc::new(AtomicUsize::new(0));
    let second_count = Arc::new(AtomicUsize::new(0));
    let (first_entered_tx, mut first_entered_rx) = tokio::sync::oneshot::channel();
    let (second_entered_tx, mut second_entered_rx) = tokio::sync::oneshot::channel();
    pool.services.lock().insert(
        "first".to_string(),
        controlled_service(
            first_entered_tx,
            Arc::clone(&first_release),
            Arc::clone(&first_count),
        ),
    );
    pool.services.lock().insert(
        "second".to_string(),
        controlled_service(
            second_entered_tx,
            Arc::clone(&second_release),
            Arc::clone(&second_count),
        ),
    );

    let first_pool = Arc::clone(&pool);
    let first_waiter = tokio::spawn(async move { first_pool.shutdown().await });
    let first_service_started = tokio::select! {
        result = &mut first_entered_rx => {
            result.expect("first close signal must arrive");
            true
        }
        result = &mut second_entered_rx => {
            result.expect("second close signal must arrive");
            false
        }
    };
    first_waiter.abort();
    first_waiter
        .await
        .expect_err("caller cancellation must win");

    let retry_pool = Arc::clone(&pool);
    let mut retry = tokio::spawn(async move { retry_pool.shutdown().await });
    if first_service_started {
        first_release.notify_one();
        tokio::select! {
            result = &mut second_entered_rx => {
                result.expect("retry must observe the same transaction reaching service two");
            }
            result = &mut retry => {
                result.expect("retry waiter must not panic");
                panic!("retry returned before the remaining service was explicitly closed");
            }
        }
    } else {
        second_release.notify_one();
        tokio::select! {
            result = &mut first_entered_rx => {
                result.expect("retry must observe the same transaction reaching service one");
            }
            result = &mut retry => {
                result.expect("retry waiter must not panic");
                panic!("retry returned before the remaining service was explicitly closed");
            }
        }
    }
    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        pool.lifecycle.load(Ordering::Acquire),
        1,
        "pool cannot publish Closed before every service settles"
    );
    if first_service_started {
        second_release.notify_one();
    } else {
        first_release.notify_one();
    }
    let report = retry.await.expect("retry waiter must complete");

    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(pool.lifecycle.load(Ordering::Acquire), 2);
    assert_eq!(
        report,
        McpPoolShutdownReport::Complete {
            settled_services: 2,
            failed_services: 0,
        }
    );
}

#[tokio::test]
async fn test_concurrent_and_repeated_shutdown_share_one_service_transaction() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = Arc::new(McpClientPool::new_empty());
    let release = Arc::new(tokio::sync::Notify::new());
    let close_count = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    pool.services.lock().insert(
        "only".to_string(),
        controlled_service(entered_tx, Arc::clone(&release), Arc::clone(&close_count)),
    );

    let first_pool = Arc::clone(&pool);
    let second_pool = Arc::clone(&pool);
    let first = tokio::spawn(async move { first_pool.shutdown().await });
    entered_rx.await.expect("service close must start");
    let second = tokio::spawn(async move { second_pool.shutdown().await });
    release.notify_one();
    let first_report = first.await.unwrap();
    let second_report = second.await.unwrap();
    let repeated_report = pool.shutdown().await;

    assert_eq!(close_count.load(Ordering::SeqCst), 1);
    assert_eq!(pool.lifecycle.load(Ordering::Acquire), 2);
    assert_eq!(first_report, second_report);
    assert_eq!(second_report, repeated_report);
}

#[tokio::test]
async fn test_timed_out_service_is_recorded_incomplete_and_never_publishes_closed() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = McpClientPool::new_empty();
    let close_count = Arc::new(AtomicUsize::new(0));
    pool.services.lock().insert(
        "unfinished".to_string(),
        timing_out_service(Arc::clone(&close_count)),
    );

    let first = pool.shutdown().await;
    let repeated = pool.shutdown().await;

    assert_eq!(
        first,
        McpPoolShutdownReport::Incomplete {
            settled_services: 0,
            unfinished_services: 1,
            failed_services: 0,
        }
    );
    assert_eq!(repeated, first);
    assert_eq!(close_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        pool.lifecycle.load(Ordering::Acquire),
        1,
        "an rmcp timeout is degraded evidence, not Closed"
    );
}

#[tokio::test]
async fn test_shutdown_clears_notifier_capture() {
    let pool = McpClientPool::new_empty();
    let captured = Arc::new(());
    let weak = Arc::downgrade(&captured);
    pool.set_notifier(Box::new(move |_| {
        let _ = &captured;
    }));

    pool.shutdown().await;

    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn test_callback_cloned_before_begin_shutdown_cannot_repopulate_pending_flow() {
    let pool = Arc::new(McpClientPool::new_empty());
    let weak_pool = Arc::downgrade(&pool);
    pool.set_oauth_event_callback(move |event| {
        let OAuthFlowEvent::AuthorizationNeeded {
            flow_id,
            server_name,
            callback_tx,
            ..
        } = event
        else {
            return;
        };
        if let Some(pool) = weak_pool.upgrade() {
            let _ = pool.register_oauth_callback(&server_name, &flow_id, callback_tx);
        }
    });
    let callback = pool
        .oauth_event_callback()
        .expect("callback must be cloned before shutdown");
    let (callback_tx, callback_rx) = tokio::sync::oneshot::channel();

    pool.begin_shutdown();
    callback(OAuthFlowEvent::AuthorizationNeeded {
        flow_id: "flow-cloned".to_string(),
        server_name: "docs".to_string(),
        authorization_url: "https://example.test/authorize".to_string(),
        callback_tx,
    });

    assert!(
        callback_rx.await.is_err(),
        "late callback sender must be dropped"
    );
    assert!(pool.active_oauth_flow("docs").is_none());
    assert!(pool
        .deliver_oauth_callback("docs", "code".into(), "state".into())
        .is_err());
}

#[tokio::test]
async fn test_blocked_oauth_task_crossing_shutdown_cannot_commit_callback_or_service() {
    use crate::mcp::{McpTaskKey, McpTaskOwner, McpTaskShutdownReport};

    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let weak_pool = Arc::downgrade(&pool);
    pool.set_oauth_event_callback(move |event| {
        let OAuthFlowEvent::AuthorizationNeeded {
            flow_id,
            server_name,
            callback_tx,
            ..
        } = event
        else {
            return;
        };
        if let Some(pool) = weak_pool.upgrade() {
            let _ = pool.register_oauth_callback(&server_name, &flow_id, callback_tx);
        }
    });
    let cloned_callback = pool.oauth_event_callback().unwrap();
    let task_pool = Arc::clone(&pool);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (callback_tx, callback_rx) = tokio::sync::oneshot::channel();
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
    let close_release = Arc::new(tokio::sync::Notify::new());
    close_release.notify_one();
    let close_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (close_entered_tx, _close_entered_rx) = tokio::sync::oneshot::channel();
    let service = controlled_service(close_entered_tx, close_release, close_count);
    pool.spawn_background(McpTaskKey::OAuth("flow-blocked".into()), async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        cloned_callback(OAuthFlowEvent::AuthorizationNeeded {
            flow_id: "flow-blocked".to_string(),
            server_name: "docs".to_string(),
            authorization_url: "https://example.test/authorize".to_string(),
            callback_tx,
        });
        let committed = match task_pool.try_commit_connection(
            "docs".to_string(),
            connected_test_handle("docs"),
            service,
        ) {
            Ok(()) => true,
            Err(mut service) => {
                let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
                false
            }
        };
        let _ = commit_tx.send(committed);
    })
    .unwrap();

    started_rx.await.unwrap();
    pool.begin_shutdown();
    release_tx.send(()).unwrap();
    assert!(
        !commit_rx.await.unwrap(),
        "Closing must reject the service commit"
    );
    assert!(
        callback_rx.await.is_err(),
        "Closing must reject late callback state"
    );
    owner.begin_shutdown();
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
    assert!(pool.services.lock().is_empty());
    assert!(pool.clients.read().is_empty());
    assert!(pool.active_oauth_flow("docs").is_none());
    assert!(pool.shutdown().await.is_complete());
}

#[tokio::test]
async fn test_concurrent_terminal_shutdown_settles_pending_task_subscription_and_service_once() {
    use crate::mcp::{McpTaskKey, McpTaskOwner, McpTaskShutdownReport};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow-terminal"),
        OAuthStartDisposition::Started
    );
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    assert!(pool.register_oauth_callback("docs", "flow-terminal", pending_tx));
    let (oauth_drop_tx, oauth_drop_rx) = tokio::sync::oneshot::channel();
    let (subscription_drop_tx, subscription_drop_rx) = tokio::sync::oneshot::channel();
    let (oauth_started_tx, oauth_started_rx) = tokio::sync::oneshot::channel();
    let (subscription_started_tx, subscription_started_rx) = tokio::sync::oneshot::channel();
    pool.spawn_background(McpTaskKey::OAuth("flow-terminal".into()), async move {
        let _guard = TestDropSignal(Some(oauth_drop_tx));
        let _ = oauth_started_tx.send(());
        std::future::pending::<()>().await;
    })
    .unwrap();
    pool.spawn_background(McpTaskKey::Subscription("docs".into()), async move {
        let _guard = TestDropSignal(Some(subscription_drop_tx));
        let _ = subscription_started_tx.send(());
        std::future::pending::<()>().await;
    })
    .unwrap();
    oauth_started_rx.await.unwrap();
    subscription_started_rx.await.unwrap();

    let release = Arc::new(tokio::sync::Notify::new());
    let close_count = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    pool.services.lock().insert(
        "docs".to_string(),
        controlled_service(entered_tx, Arc::clone(&release), Arc::clone(&close_count)),
    );

    pool.begin_shutdown();
    owner.begin_shutdown();
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
    oauth_drop_rx.await.unwrap();
    subscription_drop_rx.await.unwrap();
    assert!(pending_rx.await.is_err());

    let first_pool = Arc::clone(&pool);
    let second_pool = Arc::clone(&pool);
    let first = tokio::spawn(async move { first_pool.shutdown().await });
    entered_rx.await.unwrap();
    let second = tokio::spawn(async move { second_pool.shutdown().await });
    release.notify_one();
    let first_report = first.await.unwrap();
    let second_report = second.await.unwrap();
    let repeated_report = pool.shutdown().await;

    assert!(first_report.is_complete());
    assert_eq!(first_report, second_report);
    assert_eq!(second_report, repeated_report);
    assert_eq!(close_count.load(Ordering::SeqCst), 1);
    assert!(pool.active_oauth_flow("docs").is_none());
}
