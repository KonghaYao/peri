use super::*;
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinServerExit, BUILTIN_CONVERGE_TIMEOUT,
};
use peri_acp_types::plugin::ConfigSource;
use std::{sync::Arc, task::Poll, time::Duration};

fn pending_local_workspace() -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    let mut config: McpServerConfig = serde_json::from_str("{}").unwrap();
    config.source = Some(ConfigSource::Builtin {
        instance: "workspace".into(),
    });
    config.system_mcp = Some(true);
    pool.configs.write().insert("workspace".into(), config);
    pool
}

/// [回归测试] 恢复查询不能把本地实例装配中的短暂缺席持久化为 Unknown。
#[tokio::test]
async fn pending_workspace_waits_for_real_connection_without_waiting_for_other_servers() {
    let pool = pending_local_workspace();
    let config = pool.configs.write().remove("workspace").unwrap();
    pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(".")))
        .unwrap();
    let mut query = Box::pin(pool.workspace_live_tasks("owner-a"));
    assert!(matches!(futures::poll!(&mut query), Poll::Pending));
    pool.configs.write().insert("workspace".into(), config);
    pool.publish_system_manifest(readiness::SystemMcpManifest::Loaded);
    assert!(matches!(futures::poll!(&mut query), Poll::Pending));
    let dir = tempfile::tempdir().unwrap();
    let server = peri_mcp_workspace::WorkspaceMcpServer::standalone(
        dir.path().to_string_lossy().into_owned(),
        false,
    );
    let transport = spawn_builtin_transport_with_handler("workspace", server.clone());
    let (io, supervisor) = transport.into_parts();
    let mut service = serve_client_auto(
        io,
        &McpCapabilityProfile::disabled(),
        Duration::from_secs(5),
    )
    .await
    .expect("真实 workspace 握手必须有界")
    .expect("真实 workspace 握手必须成功");
    pool.clients.write().insert(
        "workspace".into(),
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: "workspace".into(),
            version: None,
            cache_version: None,
            connected_at: None,
            protocol_version: None,
            peer: Some(service.peer().clone()),
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(ConfigSource::Builtin {
                instance: "workspace".into(),
            }),
            url: None,
            skills_capable: false,
        }),
    );
    pool.clear_discovery_evidence("workspace");
    assert!(
        !pool.system_settled(),
        "仍未收口的其他实例不属于该只读查询的依赖"
    );
    let result = peri_time::timeout(Duration::from_secs(1), query)
        .await
        .expect("workspace 已连上就必须查询，无需等整池初始化");
    assert!(matches!(result, PeripheralTasks::Known(tasks) if tasks.is_empty()));
    pool.begin_shutdown();
    service
        .close_with_timeout(Duration::from_secs(1))
        .await
        .expect("真实连接必须正常关闭");
    let outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(!matches!(
        outcome.server,
        BuiltinServerExit::AbortedAfterTimeout
    ));
    let _ = server.shutdown_shell_tasks().await;
}

#[tokio::test]
async fn pending_workspace_failure_wakes_query_and_preserves_unknown_reason() {
    let pool = pending_local_workspace();
    let mut query = Box::pin(pool.workspace_live_tasks("owner-a"));
    assert!(matches!(futures::poll!(&mut query), Poll::Pending));
    McpClientPool::insert_failed(&pool, "workspace", "readiness-test-connect-failure".into());
    let result = peri_time::timeout(Duration::from_secs(1), query)
        .await
        .expect("明确连接失败应立即结束等待");
    assert!(
        matches!(result, PeripheralTasks::Unknown { detail } if detail.contains("readiness-test-connect-failure"))
    );
}

#[tokio::test(start_paused = true)]
async fn pending_workspace_uses_the_existing_query_deadline_and_returns_unknown() {
    let pool = pending_local_workspace();
    let started = peri_time::monotonic_now();
    let mut query = Box::pin(pool.workspace_live_tasks("owner-a"));
    assert!(matches!(futures::poll!(&mut query), Poll::Pending));
    tokio::time::advance(PERIPHERAL_QUERY_TIMEOUT).await;
    let result = query.await;
    assert!(
        matches!(result, PeripheralTasks::Unknown { detail } if detail == "shell task list query timed out after 10s")
    );
    assert_eq!(peri_time::elapsed_since(started), PERIPHERAL_QUERY_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn absent_disabled_and_remote_workspace_do_not_gain_a_startup_wait() {
    for source in [None, Some(ConfigSource::WorkspaceRemote)] {
        let pool = pending_local_workspace();
        pool.configs.write().get_mut("workspace").unwrap().source = source;
        let started = peri_time::monotonic_now();
        let result = pool.workspace_live_tasks("owner-a").await;
        assert!(matches!(result, PeripheralTasks::Unknown { .. }));
        assert_eq!(peri_time::elapsed_since(started), Duration::ZERO);
    }
    let pool = pending_local_workspace();
    pool.configs.write().get_mut("workspace").unwrap().disabled = Some(true);
    let started = peri_time::monotonic_now();
    assert!(matches!(
        pool.workspace_live_tasks("owner-a").await,
        PeripheralTasks::Unknown { .. }
    ));
    assert_eq!(peri_time::elapsed_since(started), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn configuration_failure_wakes_recovery_without_starting_a_workspace_query() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(".")))
        .unwrap();
    let started = peri_time::monotonic_now();
    let mut query = Box::pin(pool.workspace_live_tasks("owner-a"));
    assert!(matches!(futures::poll!(&mut query), Poll::Pending));
    pool.publish_system_manifest(readiness::SystemMcpManifest::Failed);
    assert!(matches!(query.await, PeripheralTasks::Unknown { .. }));
    assert_eq!(peri_time::elapsed_since(started), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn system_settled_and_pool_close_each_wake_pending_recovery_query() {
    for close in [false, true] {
        let pool = pending_local_workspace();
        let started = peri_time::monotonic_now();
        let mut query = Box::pin(pool.workspace_live_tasks("owner-a"));
        assert!(matches!(futures::poll!(&mut query), Poll::Pending));
        if close {
            pool.begin_shutdown();
        } else {
            pool.mark_system_settled();
        }
        assert!(matches!(query.await, PeripheralTasks::Unknown { .. }));
        assert_eq!(peri_time::elapsed_since(started), Duration::ZERO);
    }
}
