use super::*;
use crate::mcp::client::{ConnectionResourceCache, McpClientPool};
use crate::mcp::config::McpCachePolicy;
use crate::mcp::resource_cache::McpResourceCache;
use peri_acp_types::workspace::WorkspaceId;

pub(super) fn scoped_cache(
    handle: Arc<McpClientHandle>,
    cache: McpResourceCache,
    workspace: WorkspaceId,
    version: Option<&str>,
) -> (ConnectionResourceCache, String) {
    handle
        .peer
        .as_ref()
        .unwrap()
        .set_peer_info(rmcp::model::InitializeResult::default().into());
    let mut pool = McpClientPool::new_pending();
    pool.bind_workspace_scope(workspace).unwrap();
    pool.bind_cache_policy(McpCachePolicy::Enabled).unwrap();
    pool.resource_cache = cache;
    pool.advance_handle_generation(&handle);
    pool.clients
        .write()
        .insert(handle.name.clone(), Arc::clone(&handle));
    let origin = pool.cache_origin(&handle.name);
    if let Some(version) = version {
        pool.cache_versions
            .write()
            .insert(handle.name.clone(), version.to_string());
    }
    pool.resource_cache.set_cache_version(&origin, version);
    Arc::new(pool).resource_cache_for_handle(&handle).unwrap()
}
