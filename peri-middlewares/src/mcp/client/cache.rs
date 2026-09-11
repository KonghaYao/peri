//! Pool 的持久化缓存准入、版本 fencing、RPC 缓存包装与失效策略。

use super::service::peer_cache_version;
use super::{McpClientPool, McpConnectionKey};
use crate::mcp::config::McpServerConfig;
use rmcp::{
    model::{
        CacheScope, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult,
        Resource, Tool,
    },
    service::{Peer, RoleClient},
};

pub(crate) fn cache_scope_allows_persistence(scope: Option<CacheScope>) -> bool {
    match scope {
        Some(CacheScope::Public) | Some(CacheScope::Private) => true,
        Some(_) | None => false,
    }
}

impl McpClientPool {
    fn config_allows_persistent_cache(config: &McpServerConfig) -> bool {
        // `private` 只可在匿名上下文复用。任意静态 header、HTTP query 与
        // stdio env 都可能携带 Cookie、API key 或服务自定义凭据，保守禁用。
        config.oauth.is_none()
            && config
                .headers
                .as_ref()
                .is_none_or(std::collections::HashMap::is_empty)
            && config.url.as_deref().is_none_or(|url| !url.contains('?'))
            && config
                .env
                .as_ref()
                .is_none_or(std::collections::HashMap::is_empty)
    }

    pub(crate) fn persistent_cache_allowed(&self, server_name: &str) -> bool {
        self.persistent_cache_allowed_for(&McpConnectionKey::static_server(server_name))
    }

    pub(crate) fn persistent_cache_allowed_for(&self, connection: &McpConnectionKey) -> bool {
        if connection.is_dynamic() {
            // Dynamic MCP 首版 fail-closed：在 scoped cache ticket/current-instance
            // fencing 完成前，不读取或写入持久化 resource cache。
            return false;
        }
        self.configs
            .read()
            .get(connection.server_name())
            .is_none_or(Self::config_allows_persistent_cache)
    }

    pub(crate) fn install_peer_cache_version(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
    ) -> Option<String> {
        let cache_version = peer_cache_version(peer);
        let origin = self.cache_origin(server_name);
        self.resource_cache
            .set_cache_version(&origin, cache_version.as_deref());
        if let Some(version) = cache_version.as_ref() {
            self.cache_versions
                .write()
                .insert(server_name.to_string(), version.clone());
        } else {
            self.cache_versions.write().remove(server_name);
        }
        cache_version
    }

    pub(crate) async fn read_resource_cached(
        &self,
        server_name: &str,
        uri: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<
        (
            ReadResourceResult,
            Option<crate::mcp::resource_cache::CacheTicket>,
        ),
        rmcp::service::ServiceError,
    > {
        if !self.persistent_cache_allowed(server_name) {
            return Ok((
                peer.read_resource(ReadResourceRequestParams::new(uri))
                    .await?,
                None,
            ));
        }
        let origin = self.cache_origin(server_name);
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(result) = self
            .resource_cache
            .get_versioned(&origin, "resources/read", uri, cache_version.as_deref())
            .await
        {
            return Ok((result, None));
        }
        self.resource_cache
            .mark_live_fetch(&origin, "resources/read");
        let Some(ticket) = self
            .resource_cache
            .ticket(&origin, "resources/read", uri)
            .await
        else {
            return Ok((
                peer.read_resource(ReadResourceRequestParams::new(uri))
                    .await?,
                None,
            ));
        };
        let result = peer
            .read_resource(ReadResourceRequestParams::new(uri))
            .await?;
        Ok((result, Some(ticket)))
    }

    /// 仅由资源使用方在内容验证成功后调用。这样受 SEP-2640 内容绑定保护的
    /// `skill://` 响应不会在 digest 校验失败时落入跨进程缓存。
    pub(crate) async fn cache_verified_resource(
        &self,
        server_name: &str,
        ticket: Option<crate::mcp::resource_cache::CacheTicket>,
        result: &ReadResourceResult,
    ) {
        let Some(ticket) = ticket else { return };
        self.persist_cacheable_response(
            server_name,
            &ticket,
            result,
            result.ttl_ms,
            result.cache_scope,
        )
        .await;
    }

    pub(crate) async fn list_resources_cached(
        &self,
        server_name: &str,
        params: Option<PaginatedRequestParams>,
        peer: &Peer<RoleClient>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::service::ServiceError> {
        if !self.persistent_cache_allowed(server_name) {
            return peer.list_resources(params).await;
        }
        let origin = self.cache_origin(server_name);
        let params_key = serde_json::to_string(&params).unwrap_or_default();
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(result) = self
            .resource_cache
            .get_versioned(
                &origin,
                "resources/list",
                &params_key,
                cache_version.as_deref(),
            )
            .await
        {
            return Ok(result);
        }
        self.resource_cache
            .mark_live_fetch(&origin, "resources/list");
        let ticket = self
            .resource_cache
            .ticket(&origin, "resources/list", &params_key)
            .await;
        let result = peer.list_resources(params).await?;
        if let Some(ticket) = ticket {
            self.persist_cacheable_response(
                server_name,
                &ticket,
                &result,
                result.ttl_ms,
                result.cache_scope,
            )
            .await;
        }
        Ok(result)
    }

    pub(crate) async fn list_all_resources_cached(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<Vec<Resource>, rmcp::service::ServiceError> {
        let mut resources = Vec::new();
        let mut cursor = None;
        loop {
            let result = self
                .list_resources_cached(
                    server_name,
                    Some(PaginatedRequestParams::default().with_cursor(cursor)),
                    peer,
                )
                .await?;
            resources.extend(result.resources);
            cursor = result.next_cursor;
            if cursor.is_none() {
                return Ok(resources);
            }
        }
    }

    /// 缓存包装器供后续 Resource Template 消费者使用；当前 Agent 尚未暴露
    /// templates/list 的目录工具，因此不在初始化阶段进行无目的预取。
    pub async fn list_resource_templates_cached(
        &self,
        server_name: &str,
        params: Option<PaginatedRequestParams>,
        peer: &Peer<RoleClient>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, rmcp::service::ServiceError> {
        if !self.persistent_cache_allowed(server_name) {
            return peer.list_resource_templates(params).await;
        }
        let origin = self.cache_origin(server_name);
        let params_key = serde_json::to_string(&params).unwrap_or_default();
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(result) = self
            .resource_cache
            .get_versioned(
                &origin,
                "resources/templates/list",
                &params_key,
                cache_version.as_deref(),
            )
            .await
        {
            return Ok(result);
        }
        let ticket = self
            .resource_cache
            .ticket(&origin, "resources/templates/list", &params_key)
            .await;
        let result = peer.list_resource_templates(params).await?;
        if let Some(ticket) = ticket {
            self.persist_cacheable_response(
                server_name,
                &ticket,
                &result,
                result.ttl_ms,
                result.cache_scope,
            )
            .await;
        }
        Ok(result)
    }

    pub(crate) async fn invalidate_resource_cache(&self, server_name: &str, uri: Option<&str>) {
        let origin = self.cache_origin(server_name);
        self.invalidate_resource_cache_origin(&origin, uri).await;
    }

    /// 仅当 server 在 initialize 声明 `io.mcpp/server-cache-version` 且当前安全
    /// 策略允许持久化时，跨进程复用磁盘上的 `tools/list` schema；否则保持原始
    /// 网络行为（每次回源）。命中以协商的 cache_version 为准：同版本命中跳过
    /// 网络，版本缺失/变化必定回源。
    pub(crate) async fn list_all_tools_cached(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<Vec<Tool>, rmcp::service::ServiceError> {
        if !self.tools_cache_eligible(server_name) {
            return peer.list_all_tools().await;
        }
        let origin = self.cache_origin(server_name);
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(version) = cache_version.as_deref() {
            if let Some(tools) = self
                .resource_cache
                .get_versioned::<Vec<Tool>>(&origin, "tools/list", "", Some(version))
                .await
            {
                return Ok(tools);
            }
        }
        self.resource_cache.mark_live_fetch(&origin, "tools/list");
        let ticket = self.resource_cache.ticket(&origin, "tools/list", "").await;
        let tools = peer.list_all_tools().await?;
        if let (Some(ticket), Some(version)) = (ticket, cache_version.as_deref()) {
            self.resource_cache
                .put_ticket_versioned(&ticket, std::time::Duration::ZERO, Some(version), &tools)
                .await;
        }
        Ok(tools)
    }

    /// 跨进程复用 `tools/list` 缓存的准入：安全策略允许持久化且 server 已声明
    /// cache_version。二者任一不满足则只回源、不读盘（对应「无版本不命中」与
    /// 「安全策略回退」）。
    pub(crate) fn tools_cache_eligible(&self, server_name: &str) -> bool {
        self.persistent_cache_allowed(server_name)
            && self.cache_versions.read().contains_key(server_name)
    }

    /// `notifications/tools/list_changed` 到达时失效该 origin 的磁盘 `tools/list`
    /// 缓存。订阅未启用时由版本比对安全兜底（下次回源用新版本失效旧条目）。
    pub(crate) async fn invalidate_tools_cache(&self, server_name: &str) {
        let origin = self.cache_origin(server_name);
        self.resource_cache
            .invalidate(&origin, "tools/list", None)
            .await;
    }

    pub(crate) async fn invalidate_resource_cache_origin(&self, origin: &str, uri: Option<&str>) {
        match uri {
            Some(_uri) => {
                // 一个 resources/read 响应可包含多个 contents[] URI；当前 cache
                // 未维护反向索引，无法确认通知 URI 对应哪个聚合请求。按 MCPP
                // 7.3.2 保守失效该 origin 的 read domain，避免聚合响应继续命中。
                self.resource_cache
                    .invalidate(origin, "resources/read", None)
                    .await;
            }
            None => {
                self.resource_cache
                    .invalidate(origin, "resources/list", None)
                    .await;
                self.resource_cache
                    .invalidate(origin, "resources/templates/list", None)
                    .await;
            }
        }
    }

    pub(crate) fn cache_origin(&self, server_name: &str) -> String {
        let config = self.configs.read().get(server_name).cloned();
        crate::mcp::resource_cache::cache_origin(server_name, config.as_ref())
    }

    pub(crate) fn resource_cache(&self) -> crate::mcp::resource_cache::McpResourceCache {
        self.resource_cache.clone()
    }

    pub(super) fn cache_status_for(&self, server_name: &str) -> Option<String> {
        if !self.persistent_cache_allowed(server_name) {
            return Some("cache_disabled".to_string());
        }
        let origin = self.cache_origin(server_name);
        if let Some(status) = self.resource_cache.recent_status(&origin) {
            return Some(match status {
                crate::mcp::resource_cache::CacheLoadStatus::VersionHit => {
                    "version_cached".to_string()
                }
                crate::mcp::resource_cache::CacheLoadStatus::McppHit => "mcpp_cached".to_string(),
                crate::mcp::resource_cache::CacheLoadStatus::ResourceHit => "cached".to_string(),
                crate::mcp::resource_cache::CacheLoadStatus::LiveFetch => "live_fetch".to_string(),
                crate::mcp::resource_cache::CacheLoadStatus::StoredAfterFetch => {
                    "stored_after_fetch".to_string()
                }
            });
        }
        Some(if self.persistent_cache_allowed(server_name) {
            "cache_ready".to_string()
        } else {
            "cache_disabled".to_string()
        })
    }

    async fn persist_cacheable_response<T: serde::Serialize>(
        &self,
        server_name: &str,
        ticket: &crate::mcp::resource_cache::CacheTicket,
        result: &T,
        ttl_ms: Option<u64>,
        cache_scope: Option<CacheScope>,
    ) {
        if !self.persistent_cache_allowed(server_name) {
            return;
        }
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        let can_reuse = cache_scope_allows_persistence(cache_scope)
            || (cache_scope.is_none() && ttl_ms.is_some());
        if !can_reuse {
            return;
        }
        let ttl = std::time::Duration::from_millis(ttl_ms.unwrap_or_default());
        if !ttl.is_zero() || cache_version.is_some() {
            self.resource_cache
                .put_ticket_versioned(ticket, ttl, cache_version.as_deref(), result)
                .await;
        }
    }
}
