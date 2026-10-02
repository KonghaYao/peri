use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use rmcp::{
    model::{ReadResourceResult, Tool},
    service::RoleClient,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{McpCachePolicy, McpClientPool};

const RESOURCE_URI: &str = "test://data";
const VERSION: &str = "cache-policy-test-v1";
const SKILL_URI: &str = "skill://demo/SKILL.md";
const SKILL_TEXT: &str = "---\nname: demo\ndescription: Live description\n---\n\n# Live\n";

fn response(method: &str, marker: &str) -> Value {
    match method {
        "tools/list" => json!({"tools": [{"name": marker, "inputSchema": {"type": "object"}}]}),
        "resources/list" => {
            json!({"resources": [{"uri": format!("test://{marker}"), "name": marker}], "ttlMs": 60_000, "cacheScope": "public"})
        }
        "resources/templates/list" => {
            json!({"resourceTemplates": [{"uriTemplate": format!("test://{marker}/{{id}}"), "name": marker}], "ttlMs": 60_000, "cacheScope": "public"})
        }
        "resources/read" => {
            json!({"contents": [{"uri": RESOURCE_URI, "text": marker}], "ttlMs": 60_000, "cacheScope": "public"})
        }
        "skills/list" => {
            json!({"skills": [{"uri": SKILL_URI, "frontmatter": {"name": "demo", "description": "Live description"}}], "ttlMs": 60_000, "cacheScope": "public"})
        }
        _ => json!({}),
    }
}

async fn serve_responses(
    transport: tokio::io::DuplexStream,
    calls: Arc<Mutex<HashMap<String, usize>>>,
    failed: Arc<AtomicBool>,
) {
    let (reader, mut writer) = tokio::io::split(transport);
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        let Some(request_id) = request.get("id") else {
            continue;
        };
        let method = request["method"].as_str().unwrap();
        *calls.lock().unwrap().entry(method.into()).or_default() += 1;
        let result = if method == "resources/read" && request["params"]["uri"] == SKILL_URI {
            json!({"contents": [{"uri": SKILL_URI, "text": SKILL_TEXT}], "ttlMs": 60_000, "cacheScope": "public"})
        } else {
            response(method, "live")
        };
        let reply = if failed.load(Ordering::SeqCst) {
            json!({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32603, "message": "live RPC failed"}})
        } else {
            json!({"jsonrpc": "2.0", "id": request_id, "result": result})
        };
        if writer
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn seed_cache(pool: &McpClientPool) {
    let origin = pool.cache_origin("server");
    pool.cache_versions
        .write()
        .insert("server".into(), VERSION.into());
    pool.resource_cache
        .set_cache_version(&origin, Some(VERSION));
    for (method, params) in [
        ("tools/list", ""),
        ("resources/list", "null"),
        ("resources/templates/list", "null"),
        ("resources/read", RESOURCE_URI),
    ] {
        let ticket = pool
            .resource_cache
            .ticket(&origin, method, params)
            .await
            .unwrap();
        pool.resource_cache
            .put_ticket_versioned(
                &ticket,
                Duration::from_secs(60),
                Some(VERSION),
                &cached_payload(method),
            )
            .await;
    }
}

fn cached_payload(method: &str) -> Value {
    let response = response(method, "stale");
    if method == "tools/list" {
        response["tools"].clone()
    } else {
        response
    }
}

#[tokio::test]
async fn disabled_policy_bypasses_every_seeded_response_and_preserves_payloads() {
    let pool = McpClientPool::new_empty_with_cache_policy(McpCachePolicy::Disabled);
    seed_cache(&pool).await;
    let calls = Arc::new(Mutex::new(HashMap::new()));
    let failed = Arc::new(AtomicBool::new(false));
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(serve_responses(server_io, calls.clone(), failed.clone()));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let peer = running.peer();
    pool.configure_peer_cache(peer).await;
    assert!(!peer.response_cache_config().await.enabled);
    for _ in 0..2 {
        let tools = pool.list_all_tools_cached("server", peer).await.unwrap();
        assert_eq!(tools[0].name, "live");
        let resources = pool
            .list_resources_cached("server", None, peer)
            .await
            .unwrap();
        assert_eq!(resources.resources[0].name, "live");
        let templates = pool
            .list_resource_templates_cached("server", None, peer)
            .await
            .unwrap();
        assert_eq!(templates.resource_templates[0].name, "live");
        let (resource, ticket) = pool
            .read_resource_cached("server", RESOURCE_URI, peer)
            .await
            .unwrap();
        assert!(ticket.is_none());
        assert_eq!(
            serde_json::to_value(&resource).unwrap()["contents"][0]["text"],
            "live"
        );
        pool.cache_verified_resource("server", ticket, &resource)
            .await;
    }
    let origin = pool.cache_origin("server");
    for (method, params) in [
        ("tools/list", ""),
        ("resources/list", "null"),
        ("resources/templates/list", "null"),
        ("resources/read", RESOURCE_URI),
    ] {
        assert_eq!(calls.lock().unwrap()[method], 2);
        let cached: Value = pool
            .resource_cache
            .get_versioned(&origin, method, params, Some(VERSION))
            .await
            .unwrap();
        assert_eq!(cached, cached_payload(method));
    }
    assert_eq!(
        pool.cache_status_for("server").as_deref(),
        Some("cache_disabled_by_config")
    );
    failed.store(true, Ordering::SeqCst);
    assert!(pool.list_all_tools_cached("server", peer).await.is_err());
    assert!(pool
        .read_resource_cached("server", RESOURCE_URI, peer)
        .await
        .is_err());
    assert!(pool
        .list_resources_cached("server", None, peer)
        .await
        .is_err());
    assert!(pool
        .list_resource_templates_cached("server", None, peer)
        .await
        .is_err());
    running.cancel().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn enabled_policy_preserves_versioned_cache_hits_without_rpc() {
    let pool = McpClientPool::new_empty();
    seed_cache(&pool).await;
    let (client_io, server_io) = tokio::io::duplex(8192);
    let calls = Arc::new(Mutex::new(HashMap::new()));
    let server = tokio::spawn(serve_responses(
        server_io,
        calls.clone(),
        Arc::new(AtomicBool::new(false)),
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let peer = running.peer();
    assert_eq!(
        pool.list_all_tools_cached("server", peer).await.unwrap()[0].name,
        "stale"
    );
    assert_eq!(
        pool.list_resources_cached("server", None, peer)
            .await
            .unwrap()
            .resources[0]
            .name,
        "stale"
    );
    assert_eq!(
        pool.list_resource_templates_cached("server", None, peer)
            .await
            .unwrap()
            .resource_templates[0]
            .name,
        "stale"
    );
    let (resource, ticket) = pool
        .read_resource_cached("server", RESOURCE_URI, peer)
        .await
        .unwrap();
    assert!(ticket.is_none());
    assert_eq!(
        serde_json::to_value(resource).unwrap()["contents"][0]["text"],
        "stale"
    );
    assert!(calls.lock().unwrap().is_empty());
    running.cancel().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn disabled_policy_keeps_notification_invalidation_effective() {
    let pool = McpClientPool::new_empty_with_cache_policy(McpCachePolicy::Disabled);
    seed_cache(&pool).await;
    pool.invalidate_tools_cache("server").await;
    pool.invalidate_resource_cache("server", Some(RESOURCE_URI))
        .await;
    let origin = pool.cache_origin("server");
    let tools: Option<Vec<Tool>> = pool
        .resource_cache
        .get_versioned(&origin, "tools/list", "", Some(VERSION))
        .await;
    let resource: Option<ReadResourceResult> = pool
        .resource_cache
        .get_versioned(&origin, "resources/read", RESOURCE_URI, Some(VERSION))
        .await;
    assert!(tools.is_none());
    assert!(resource.is_none());
}

#[test]
fn pending_policy_is_fail_closed_and_bound_policy_cannot_change() {
    let pool = McpClientPool::new_pending();
    assert!(!pool.persistent_cache_allowed("server"));
    assert_eq!(
        pool.cache_status_for("server").as_deref(),
        Some("cache_disabled_workspace_unbound")
    );
    pool.bind_cache_policy(McpCachePolicy::Disabled).unwrap();
    pool.bind_cache_policy(McpCachePolicy::Disabled).unwrap();
    assert!(pool.bind_cache_policy(McpCachePolicy::Enabled).is_err());
    pool.cache_versions
        .write()
        .insert("server".into(), VERSION.into());
    assert!(!pool.tools_cache_eligible("server"));
    assert!(!pool.persistent_cache_allowed("server"));
}

#[test]
fn persistent_cache_origin_is_partitioned_by_workspace_identity() {
    use peri_acp_types::workspace::WorkspaceId;

    let workspace_a = WorkspaceId::new();
    let workspace_b = WorkspaceId::new();
    let pools = [
        McpClientPool::new_pending(),
        McpClientPool::new_pending(),
        McpClientPool::new_pending(),
    ];
    for (pool, workspace) in pools.iter().zip([workspace_a, workspace_a, workspace_b]) {
        pool.bind_workspace_scope(workspace).unwrap();
        pool.bind_cache_policy(McpCachePolicy::Enabled).unwrap();
        assert!(pool.persistent_cache_allowed("server"));
    }
    assert_eq!(
        pools[0].cache_origin("server"),
        pools[1].cache_origin("server")
    );
    assert_ne!(
        pools[0].cache_origin("server"),
        pools[2].cache_origin("server")
    );
}

#[tokio::test]
async fn disabled_policy_bypasses_skill_discovery_caches_and_keeps_registry_live() {
    use super::{ClientStatus, McpClientHandle, OAuthStatus};
    use peri_acp_types::mcp_skills::McpSkillRegistry;
    use rmcp::model::Resource;
    use tokio_util::sync::CancellationToken;

    for skills_capable in [true, false] {
        let pool = Arc::new(McpClientPool::new_empty_with_cache_policy(
            McpCachePolicy::Disabled,
        ));
        let origin = pool.cache_origin("server");
        let params = if skills_capable { "{}" } else { SKILL_URI };
        let method = if skills_capable {
            "skills/list"
        } else {
            "skills/legacy-read"
        };
        let stale = if skills_capable {
            json!({"skills": [{"uri": SKILL_URI, "frontmatter": {"name": "demo", "description": "Stale description"}}]})
        } else {
            json!({"contents": [{"uri": SKILL_URI, "text": SKILL_TEXT.replace("Live", "Stale")}], "ttlMs": 60_000, "cacheScope": "public"})
        };
        pool.resource_cache
            .put(&origin, method, params, Duration::from_secs(60), &stale)
            .await;
        let (client_io, server_io) = tokio::io::duplex(8192);
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let server = tokio::spawn(serve_responses(
            server_io,
            calls.clone(),
            Arc::new(AtomicBool::new(false)),
        ));
        let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
            (),
            client_io,
            None::<rmcp::model::ServerPeerInfo>,
        );
        pool.configure_peer_cache(running.peer()).await;
        pool.clients.write().insert(
            "server".into(),
            Arc::new(McpClientHandle {
                name: "server".into(),
                version: None,
                cache_version: None,
                peer: Some(running.peer().clone()),
                tools: vec![],
                resources: vec![serde_json::from_value::<Resource>(
                    json!({"uri": SKILL_URI, "name": "demo"}),
                )
                .unwrap()],
                status: ClientStatus::Connected,
                oauth_status: OAuthStatus::None,
                source: None,
                url: None,
                skills_capable,
            }),
        );
        let registry = Arc::new(McpSkillRegistry::new());
        let completed = Arc::new(tokio::sync::Notify::new());
        let signal = completed.clone();
        registry.set_on_change(Some(Arc::new(move || signal.notify_one())));
        let cancelled = CancellationToken::new();
        crate::mcp::middleware::run_ensure_discovery(
            &pool,
            Some(&registry),
            None,
            None,
            &cancelled,
        );
        tokio::time::timeout(Duration::from_secs(2), completed.notified())
            .await
            .unwrap();
        let skills = registry.all_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Live description");
        assert_eq!(
            calls.lock().unwrap()[if skills_capable {
                "skills/list"
            } else {
                "resources/read"
            }],
            1
        );
        let cached: Value = pool
            .resource_cache
            .get(&origin, method, params)
            .await
            .unwrap();
        assert_eq!(cached, stale);
        running.cancel().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}
