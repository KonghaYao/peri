use std::{sync::Arc, time::Duration};

use rmcp::{
    model::{CustomRequest, CustomResult, ReadResourceRequestParams, ReadResourceResponse},
    service::{RequestContext, RoleServer},
    ServerHandler,
};
use tokio::sync::Notify;

use super::super::{output_store::tests::Wire, AcpConnectionDeclaration, McpClientPool};

struct DelayedProvider {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl DelayedProvider {
    async fn respond(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

impl ServerHandler for DelayedProvider {
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, rmcp::ErrorData> {
        self.respond().await;
        Ok(ReadResourceResponse::Complete(
            serde_json::from_value(serde_json::json!({
                    "contents": [{"uri": request.uri, "text": "---\nname: old\ndescription: old generation\n---\n\nold"}],
                "ttlMs": 60000, "cacheScope": "public"
            }))
            .unwrap(),
        ))
    }

    async fn on_custom_request(
        &self,
        _request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, rmcp::ErrorData> {
        self.respond().await;
        Ok(CustomResult::new(serde_json::json!({
            "skills": [{"uri": "skill://old/SKILL.md", "frontmatter": {"name": "old", "description": "old generation"}}],
            "ttlMs": 60000, "cacheScope": "public"
        })))
    }
}

fn install_acp(wire: &Wire, pool: &McpClientPool, connection: &str) {
    wire.install(pool, "server", false);
    pool.acp_owners
        .write()
        .insert("server".into(), "session".into());
    pool.acp_connections.write().insert(
        "server".into(),
        AcpConnectionDeclaration {
            session_id: "session".into(),
            connection_id: connection.into(),
        },
    );
}

#[tokio::test]
async fn cloned_peer_proves_the_same_registered_connection_but_another_peer_does_not() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let wire = Wire::connect(DelayedProvider {
        entered: entered.clone(),
        release: release.clone(),
    })
    .await;
    let other = Wire::connect(DelayedProvider { entered, release }).await;
    let pool = Arc::new(McpClientPool::new_empty());
    install_acp(&wire, &pool, "connection");
    let cloned = wire.client.peer().clone();
    assert!(Arc::ptr_eq(
        &wire.client.peer().peer_info().unwrap(),
        &cloned.peer_info().unwrap()
    ));
    let request = pool.capture_cache_connection("server", &cloned).unwrap();
    assert!(pool.connection_cache_allowed(&request));
    assert!(pool
        .capture_cache_connection("server", other.client.peer())
        .is_err());
    pool.clients.write().clear();
    wire.close().await;
    other.close().await;
}

#[tokio::test]
async fn unregistered_peer_is_uncached_even_with_an_enabled_static_policy() {
    let wire = Wire::connect(DelayedProvider {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    })
    .await;
    let pool = McpClientPool::new_empty();
    let request = pool
        .capture_cache_connection("server", wire.client.peer())
        .unwrap();
    assert!(!pool.connection_cache_allowed(&request));
    wire.close().await;
}

#[tokio::test]
async fn captured_resource_response_and_verified_ticket_cannot_cross_connection_generations() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let wire = Wire::connect(DelayedProvider {
        entered: entered.clone(),
        release: release.clone(),
    })
    .await;
    wire.client
        .peer()
        .set_response_cache_config(rmcp::service::ClientCacheConfig::disabled())
        .await;
    let pool = Arc::new(McpClientPool::new_empty());
    install_acp(&wire, &pool, "first");
    let task_pool = pool.clone();
    let peer = wire.client.peer().clone();
    let request = tokio::spawn(async move {
        task_pool
            .read_resource_cached("server", "test://data", &peer)
            .await
    });
    entered.notified().await;
    install_acp(&wire, &pool, "second");
    release.notify_one();
    assert!(request.await.unwrap().is_err());
    let origin = pool.cache_origin("server");
    assert!(pool
        .resource_cache
        .get_json::<serde_json::Value>(&origin, "resources/read", "test://data")
        .await
        .is_none());

    let task_pool = pool.clone();
    let peer = wire.client.peer().clone();
    let request = tokio::spawn(async move {
        task_pool
            .read_resource_cached("server", "test://data", &peer)
            .await
    });
    entered.notified().await;
    release.notify_one();
    let (result, ticket) = request.await.unwrap().unwrap();
    assert!(ticket.is_some());
    install_acp(&wire, &pool, "third");
    pool.cache_verified_resource("server", ticket, &result)
        .await;
    assert!(pool
        .resource_cache
        .get_json::<serde_json::Value>(&origin, "resources/read", "test://data")
        .await
        .is_none());
    pool.clients.write().clear();
    wire.close().await;
}

#[tokio::test]
async fn scoped_skill_and_legacy_cache_rejects_old_tickets_and_keeps_same_generation_hits() {
    let wire = Wire::connect(DelayedProvider {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    install_acp(&wire, &pool, "first");
    let handle = pool.get_client("server").unwrap();
    let (cache, origin) = pool.resource_cache_for_handle(&handle).unwrap();
    for method in ["skills/list", "skills/legacy-read"] {
        let ticket = cache.ticket(&origin, method, "null").await.unwrap();
        cache
            .put_ticket(
                &ticket,
                Duration::from_secs(60),
                &serde_json::json!({"same": true}),
            )
            .await;
        assert_eq!(
            cache
                .get_json::<serde_json::Value>(&origin, method, "null")
                .await,
            Some(serde_json::json!({"same": true}))
        );
    }
    let old_ticket = cache.ticket(&origin, "skills/list", "old").await.unwrap();
    install_acp(&wire, &pool, "second");
    assert!(pool.resource_cache_for_handle(&handle).is_none());
    assert!(cache
        .get_json::<serde_json::Value>(&origin, "skills/list", "null")
        .await
        .is_none());
    cache
        .put_ticket(
            &old_ticket,
            Duration::from_secs(60),
            &serde_json::json!({"old": true}),
        )
        .await;
    assert!(pool
        .resource_cache
        .get_json::<serde_json::Value>(&origin, "skills/list", "old")
        .await
        .is_none());
    pool.clients.write().clear();
    wire.close().await;
}

#[tokio::test]
async fn delayed_skill_and_legacy_discovery_cannot_publish_or_cache_a_retired_connection() {
    for skills_capable in [true, false] {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let wire = Wire::connect(DelayedProvider {
            entered: entered.clone(),
            release: release.clone(),
        })
        .await;
        let pool = Arc::new(McpClientPool::new_empty());
        install_acp(&wire, &pool, "first");
        {
            let mut clients = pool.clients.write();
            let handle = Arc::make_mut(clients.get_mut("server").unwrap());
            handle.skills_capable = skills_capable;
            handle.resources = vec![rmcp::model::Resource::new("skill://old/SKILL.md", "old")];
        }
        let handle = pool.get_client("server").unwrap();
        pool.advance_handle_generation(&handle);
        let cache = pool.resource_cache_for_handle(&handle).unwrap();
        let old_origin = cache.1.clone();
        let registry = Arc::new(peri_acp_types::mcp_skills::McpSkillRegistry::new());
        let token: peri_acp_types::mcp_skills::HandleToken = handle.clone();
        registry.mark_discovery_started("server", token.clone());
        let task_registry = registry.clone();
        let task = tokio::spawn(async move {
            crate::mcp::skill_discovery::run_discovery_with_cache(
                task_registry,
                None,
                handle,
                token,
                tokio_util::sync::CancellationToken::new(),
                Some(cache),
                false,
            )
            .await;
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        install_acp(&wire, &pool, "second");
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(registry.all_skills().is_empty());
        assert!(!matches!(
            registry.discovery_state("server"),
            Some(peri_acp_types::mcp_skills::ServerDiscoveryState::Discovered { .. })
        ));
        let (method, params) = if skills_capable {
            ("skills/list", "null")
        } else {
            ("skills/legacy-read", "skill://old/SKILL.md")
        };
        for origin in [old_origin, pool.cache_origin("server")] {
            assert!(pool
                .resource_cache
                .get_json::<serde_json::Value>(&origin, method, params)
                .await
                .is_none());
        }
        pool.clients.write().clear();
        wire.close().await;
    }
}
