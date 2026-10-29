//! Tests for client

use super::*;
use crate::mcp::builtin::{
    context::{BuiltinContextError, BuiltinInstanceContext},
    runtime::BuiltinSpawnError,
};
use crate::mcp::oauth_flow::OAuthFailureKind;
use peri_acp_types::plugin::McpSubscriptionsConfig;
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
        call_gate: Default::default(),
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
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

#[test]
fn task_owner_metadata_is_scoped_to_workspace_sources() {
    let pool = McpClientPool::new_pending();
    assert!(
        pool.task_owner_meta_for("workspace", "session-a").is_none(),
        "unknown source must not receive Peri metadata"
    );
    let mut builtin = connected_test_handle("workspace");
    Arc::get_mut(&mut builtin).unwrap().source = Some(crate::mcp::config::ConfigSource::Builtin {
        instance: "workspace".into(),
    });
    pool.clients.write().insert("workspace".into(), builtin);
    let meta = pool
        .task_owner_meta_for("workspace", "session-a")
        .expect("builtin workspace owner");
    assert_eq!(
        meta.0
             .0
            .get(peri_mcp_core::task_owner::TASK_OWNER_META_KEY),
        Some(&serde_json::json!("session-a"))
    );
}

#[test]
fn task_owner_metadata_covers_remote_workspace_and_skips_other_sources() {
    let pool = McpClientPool::new_pending();
    let mut remote = connected_test_handle("remote");
    Arc::get_mut(&mut remote).unwrap().source =
        Some(crate::mcp::config::ConfigSource::WorkspaceRemote);
    pool.clients.write().insert("remote".into(), remote);
    let meta = pool
        .task_owner_meta_for("remote", "session-a")
        .expect("remote workspace owner");
    assert_eq!(
        meta.0
             .0
            .get(peri_mcp_core::task_owner::TASK_OWNER_META_KEY),
        Some(&serde_json::json!("session-a"))
    );

    let mut web = connected_test_handle("web");
    Arc::get_mut(&mut web).unwrap().source = Some(crate::mcp::config::ConfigSource::Builtin {
        instance: "web".into(),
    });
    pool.clients.write().insert("web".into(), web);
    assert!(
        pool.task_owner_meta_for("web", "session-a").is_none(),
        "non-workspace servers stay generic"
    );
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
async fn test_server_infos_projects_version_and_connected_at() {
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "s".into(),
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: "s".into(),
            version: Some("1.2.3".into()),
            connected_at: Some("2026-10-11T06:32:05Z".into()),
            protocol_version: Some("2026-07-28".into()),
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
    let info = &pool.server_infos()[0];
    assert_eq!(info.version.as_deref(), Some("1.2.3"));
    assert_eq!(info.connected_at.as_deref(), Some("2026-10-11T06:32:05Z"));
    assert_eq!(info.protocol_version.as_deref(), Some("2026-07-28"));
    // ACP 端口投影同样透传（`mcp/list` 数据源）。
    let projected = peri_acp_types::ports::McpPoolPort::server_infos(&pool).unwrap();
    assert_eq!(projected[0].version.as_deref(), Some("1.2.3"));
    assert_eq!(
        projected[0].connected_at.as_deref(),
        Some("2026-10-11T06:32:05Z")
    );
    assert_eq!(projected[0].protocol_version.as_deref(), Some("2026-07-28"));
}

/// `mcp/list` 投影的不可信文本边界：条目最多 64、控制字符清洗、空描述归一。
#[test]
fn test_server_infos_projection_bounds_and_sanitizes_untrusted_text() {
    let pool = McpClientPool::new_pending();
    let mut tools = Vec::new();
    for index in 0..70 {
        let mut tool = rmcp::model::Tool::default();
        tool.name = format!("tool-{index}").into();
        if index == 0 {
            tool.description = Some("line\nbreak\ttext".into());
        }
        if index == 1 {
            // 超长文本（200 字符）必须截断到 MCP_LIST_MAX_TEXT（160）。
            tool.description = Some("d".repeat(200).into());
        }
        tools.push(tool);
    }
    let resources = (0..70)
        .map(|index| {
            let resource = rmcp::model::Resource::new(
                format!("agent://builtin/a{index}/agent.md"),
                format!("resource-{index}"),
            );
            if index == 0 {
                resource.with_description("")
            } else {
                resource
            }
        })
        .collect();
    let mut handle = (*connected_test_handle("s")).clone();
    handle.tools = tools;
    handle.resources = resources;
    pool.clients.write().insert("s".into(), Arc::new(handle));
    pool.configs.write().insert(
        "s".into(),
        serde_json::from_value(serde_json::json!({
            "command": "npx",
            "subscriptions": {
                "resources": (0..70)
                    .map(|index| format!("file:///r{index}.md"))
                    .collect::<Vec<_>>(),
                "toolsListChanged": true,
            }
        }))
        .expect("测试配置必须可解析"),
    );

    let projected = peri_acp_types::ports::McpPoolPort::server_infos(&pool).unwrap();
    let server = &projected[0];
    // 全量计数保留；投影条目有界。
    assert_eq!(server.tool_count, 70);
    assert_eq!(server.resource_count, 70);
    assert_eq!(server.tools.len(), 64);
    assert_eq!(server.resources.len(), 64);
    // 控制字符替换为空格后不再含控制字符；空描述为 None。
    let description = server.tools[0].description.as_deref().unwrap();
    assert!(!description.chars().any(char::is_control));
    assert!(description.contains("line break"));
    // 超长文本按字符数截断到 160，避免无界载荷进入周期轮询。
    let truncated = server.tools[1].description.as_deref().unwrap();
    assert_eq!(truncated.chars().count(), 160, "描述必须截断到 160 字符");
    assert!(truncated.chars().all(|ch| ch == 'd'));
    assert_eq!(server.resources[0].description, None);
    // 订阅资源同样有界；标志位保留。
    let subscriptions = server.subscriptions.as_ref().unwrap();
    assert_eq!(subscriptions.resources.len(), 64);
    assert!(subscriptions.tools_list_changed);
}
#[tokio::test]
async fn test_insert_failed() {
    let pool = Arc::new(McpClientPool::new_pending());
    McpClientPool::insert_failed(&pool, "s", "err".into());
    assert_eq!(
        pool.server_infos()[0].status,
        ClientStatus::Failed("err".into())
    );
    // ACP 端口投影（`mcp/list` 数据源）透传有界失败摘要；无配置 url 为 None。
    let projected = peri_acp_types::ports::McpPoolPort::server_infos(pool.as_ref()).unwrap();
    assert_eq!(projected[0].error_summary.as_deref(), Some("err"));
    assert_eq!(projected[0].url, None);
}
#[tokio::test]
async fn test_remove_server() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.clients.write().insert(
        "a".into(),
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: "a".into(),
            version: None,
            connected_at: None,
            protocol_version: None,
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
            call_gate: Default::default(),
            name: "s".into(),
            version: None,
            connected_at: None,
            protocol_version: None,
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
    pool.reserve_oauth_flow("missing", "flow-1");
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
fn test_server_info_preserves_multiline_failed_status() {
    let status = ClientStatus::Failed(
        "request failed: https://example.test/mcp?token=top-secret\ncaused by: verbose trace"
            .to_string(),
    );

    assert_eq!(mcp_status_label(&status), "failed");
    let summary = mcp_error_summary(&status).unwrap();
    assert_eq!(
        summary,
        "request failed: https://example.test/mcp?token=top-secret\ncaused by: verbose trace"
    );
    assert!(summary.contains("top-secret"));
    assert!(summary.contains("verbose trace"));
}

#[test]
fn test_mcp_error_summary_keeps_assignments_and_limits_length() {
    assert_eq!(
        mcp_error_summary(&ClientStatus::Failed(
            "connection failed token=top-secret".into()
        ))
        .unwrap(),
        "connection failed token=top-secret"
    );
    let summary = mcp_error_summary(&ClientStatus::Failed("错".repeat(200))).unwrap();
    assert_eq!(summary.chars().count(), 160);
}

#[path = "client_builtin_context_test.rs"]
mod builtin_context_tests;

#[path = "client_shutdown_test.rs"]
mod shutdown_tests;

/// [M9] list→read 不一致时**只**重采集一次：第二次成功即返回结果。
#[tokio::test]
async fn single_recollect_retries_once_and_returns_the_second_result() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result: Result<&str, String> =
        super::collect_with_single_recollect("workspace instruction face", || {
            let calls = std::sync::Arc::clone(&calls);
            async move {
                match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => Err("listed resource could not be read".to_string()),
                    _ => Ok("collected"),
                }
            }
        })
        .await;
    assert_eq!(result, Ok("collected"));
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "首次失败后恰好重采集一次"
    );
}

/// [M9] 持续抖动（两次都失败）：返回显式、可重试的内容准入错误，
/// 不提交半套采集结果。
#[tokio::test]
async fn single_recollect_fails_closed_with_retryable_admission_error() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result: Result<&str, String> =
        super::collect_with_single_recollect("workspace instruction face", || {
            let calls = std::sync::Arc::clone(&calls);
            async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("resources/read failed".to_string())
            }
        })
        .await;
    let error = result.expect_err("持续不一致必须失败");
    assert!(
        error.contains("retry the session admission"),
        "错误必须明说可重试：{error}"
    );
    assert!(
        error.contains("inconsistent"),
        "错误必须是内容准入错误：{error}"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "不稳定时不无限重试"
    );
}

/// [M9] 首次即成功：不触发任何重采集。
#[tokio::test]
async fn single_recollect_never_retries_a_consistent_first_pass() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result: Result<&str, String> =
        super::collect_with_single_recollect("workspace instruction face", || {
            let calls = std::sync::Arc::clone(&calls);
            async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("collected")
            }
        })
        .await;
    assert_eq!(result, Ok("collected"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// 规范 skills server：responds to `skills/list` over an rmcp duplex wire.
async fn skill_peer(
    skill: Option<&'static str>,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (reader, mut writer) = tokio::io::split(server_io);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            let trimmed = line.trim_end_matches(['\r', '\n']);
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                line.clear();
                continue;
            };
            line.clear();
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let method = parsed
                .get("method")
                .and_then(|method| method.as_str())
                .unwrap_or_default();
            let response = match (method, skill) {
                ("skills/list", Some(name)) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "skills": [{
                            "uri": format!("skill://{name}/SKILL.md"),
                            "frontmatter": { "name": name, "description": format!("{name} skill") },
                            "resources": [{
                                "uri": format!("skill://{name}/SKILL.md"),
                                "digest": format!("sha256:{}", "0".repeat(64)),
                            }],
                        }]
                    }
                }),
                ("skills/list", None) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": "no skills" }
                }),
                _ => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "method not found" }
                }),
            };
            writer
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            writer.write_all(b"\n").await.unwrap();
        }
    });
    rmcp::service::serve_directly::<rmcp::service::RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    )
}

fn skill_handle(
    name: &str,
    running: &rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    source: peri_acp_types::plugin::ConfigSource,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: Some(source),
        url: None,
        skills_capable: true,
    })
}

#[tokio::test]
async fn skills_catalog_aggregates_by_capability_across_server_names() {
    let workspace_peer = skill_peer(Some("beta")).await;
    let sandbox_peer = skill_peer(Some("alpha")).await;
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "workspace".into(),
        skill_handle(
            "workspace",
            &workspace_peer,
            crate::mcp::config::ConfigSource::Builtin {
                instance: "workspace".into(),
            },
        ),
    );
    pool.clients.write().insert(
        "sandbox".into(),
        skill_handle(
            "sandbox",
            &sandbox_peer,
            crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp")),
        ),
    );
    let catalog = pool
        .read_skills_catalog("session-a")
        .await
        .expect("catalog");
    assert_eq!(
        catalog
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp__sandbox__alpha", "mcp__workspace__beta"],
        "skills are aggregated from every skills-capable server, sorted by full name"
    );
    assert_eq!(
        catalog[0].origin,
        Some(peri_acp_types::skills::SkillOrigin::Mcp {
            server: "sandbox".into(),
            uri: "skill://alpha/SKILL.md".into(),
        })
    );
}

#[tokio::test]
async fn non_system_skill_failure_is_tolerated_but_system_workspace_fails_closed() {
    let workspace_peer = skill_peer(Some("beta")).await;
    let failing_peer = skill_peer(None).await;
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "workspace".into(),
        skill_handle(
            "workspace",
            &workspace_peer,
            crate::mcp::config::ConfigSource::Builtin {
                instance: "workspace".into(),
            },
        ),
    );
    pool.clients.write().insert(
        "sandbox".into(),
        skill_handle(
            "sandbox",
            &failing_peer,
            crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp")),
        ),
    );
    let catalog = pool
        .read_skills_catalog("session-a")
        .await
        .expect("non-system failure must not abort the catalog");
    assert_eq!(
        catalog
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp__workspace__beta"]
    );

    let failing_workspace_peer = skill_peer(None).await;
    let sandbox_peer = skill_peer(Some("alpha")).await;
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "workspace".into(),
        skill_handle(
            "workspace",
            &failing_workspace_peer,
            crate::mcp::config::ConfigSource::Builtin {
                instance: "workspace".into(),
            },
        ),
    );
    pool.clients.write().insert(
        "sandbox".into(),
        skill_handle(
            "sandbox",
            &sandbox_peer,
            crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp")),
        ),
    );
    let error = pool
        .read_skills_catalog("session-a")
        .await
        .expect_err("system workspace failure must fail closed");
    assert!(error.contains("workspace"), "{error}");
}

#[test]
fn remote_sandbox_is_not_a_local_resource_source() {
    assert!(!crate::mcp::builtin::is_local_resource_source(Some(
        &crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp"))
    )));
    assert!(!crate::mcp::builtin::is_local_resource_source(None));
    assert!(crate::mcp::builtin::is_local_resource_source(Some(
        &crate::mcp::config::ConfigSource::Builtin {
            instance: "sandbox".into(),
        }
    )));
}
