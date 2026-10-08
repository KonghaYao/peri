use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use rmcp::{
    model::{ListToolsResult, PaginatedRequestParams, ServerPeerInfo, Tool},
    service::{RequestContext, RoleServer},
    ServerHandler,
};
use tokio::sync::Notify;

use super::super::{output_store::tests::Wire, McpClientPool};
use crate::mcp::config::McpServerConfig;

struct StartupProvider {
    calls: Arc<AtomicUsize>,
    delay: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl ServerHandler for StartupProvider {
    fn get_info(&self) -> ServerPeerInfo {
        let mut info = ServerPeerInfo::from(rmcp::model::InitializeResult::default());
        info.capabilities.extensions = Some(
            serde_json::from_value(serde_json::json!({
                "io.mcpp/server-cache-version": {"cacheVersion": "startup-v1"}
            }))
            .unwrap(),
        );
        info
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((entered, release)) = &self.delay {
            entered.notify_one();
            release.notified().await;
        }
        Ok(ListToolsResult {
            tools: vec![Tool::new(
                "startup-tool",
                "startup",
                Arc::new(serde_json::Map::new()),
            )],
            ..Default::default()
        })
    }
}

fn configuration(pool: &McpClientPool) -> McpServerConfig {
    let config: McpServerConfig =
        serde_json::from_value(serde_json::json!({"command": "fixture"})).unwrap();
    pool.configs.write().insert("server".into(), config.clone());
    config
}

#[tokio::test]
async fn admitted_startup_peer_reuses_versioned_tools_across_precommit_generations() {
    let calls = Arc::new(AtomicUsize::new(0));
    let first = Wire::connect(StartupProvider {
        calls: calls.clone(),
        delay: None,
    })
    .await;
    let second = Wire::connect(StartupProvider {
        calls: calls.clone(),
        delay: None,
    })
    .await;
    let pool = McpClientPool::new_empty();
    let config = configuration(&pool);
    pool.install_peer_cache_version("server", first.client.peer());
    let normal = pool
        .capture_cache_connection("server", first.client.peer())
        .unwrap();
    assert!(!pool.connection_cache_allowed(&normal));
    let tools = pool
        .list_all_tools_cached_for_startup("server", first.client.peer(), &config)
        .await
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    first.install(&pool, "server", false);
    pool.install_peer_cache_version("server", second.client.peer());
    assert!(pool
        .list_all_tools_cached("server", second.client.peer())
        .await
        .is_err());
    let tools = pool
        .list_all_tools_cached_for_startup("server", second.client.peer(), &config)
        .await
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut foreign = config.clone();
    foreign.command = Some("different-fixture".into());
    assert!(pool
        .capture_startup_cache_connection("server", second.client.peer(), &foreign)
        .is_err());
    let mut unknown_acp = config.clone();
    unknown_acp.source = Some(crate::mcp::config::ConfigSource::Acp);
    pool.configs
        .write()
        .insert("server".into(), unknown_acp.clone());
    assert!(pool
        .capture_startup_cache_connection("server", second.client.peer(), &unknown_acp)
        .is_err());
    pool.clients.write().clear();
    first.close().await;
    second.close().await;
}

#[tokio::test]
async fn startup_response_is_rejected_when_its_admission_baseline_is_replaced() {
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let wire = Wire::connect(StartupProvider {
        calls,
        delay: Some((entered.clone(), release.clone())),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    let config = configuration(&pool);
    pool.install_peer_cache_version("server", wire.client.peer());
    let origin = pool.cache_origin("server");
    let task_pool = pool.clone();
    let peer = wire.client.peer().clone();
    let task = tokio::spawn(async move {
        task_pool
            .list_all_tools_cached_for_startup("server", &peer, &config)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    wire.install(&pool, "server", false);
    release.notify_one();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(pool
        .resource_cache
        .get_versioned::<Vec<Tool>>(&origin, "tools/list", "", Some("startup-v1"))
        .await
        .is_none());
    pool.clients.write().clear();
    wire.close().await;
}
