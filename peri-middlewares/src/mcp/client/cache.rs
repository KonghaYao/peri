//! Pool 的持久化缓存准入、版本 fencing、RPC 缓存包装与失效策略。

use super::service::peer_cache_version;
use super::{McpClientPool, McpConnectionKey};
#[path = "cache_connection.rs"]
mod connection;
pub(crate) use connection::ConnectionResourceCache;
use connection::{CacheConnection, ConnectionCacheTicket};
#[cfg(test)]
#[path = "cache_fencing_test.rs"]
mod fencing_tests;
#[cfg(test)]
#[path = "cache_startup_test.rs"]
mod startup_tests;
use crate::mcp::config::{McpCachePolicy, McpServerConfig};
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

enum CacheDenial {
    Pending,
    WorkspaceUnbound,
    Configuration,
    DynamicConnection,
    /// 会话级 ACP 连接身份不可证明 / 与请求代不一致（M7，fail-closed）。
    AcpConnection,
    Credentials,
}

/// 会话级 ACP 连接的 cache 身份判定（M7）。
enum AcpCacheScope {
    /// 该名不是会话级 ACP 连接（配置来源 / dynamic 投影的既有路径）。
    NotAcp,
    /// 是 ACP 连接且身份可证明：声明会话 + 连接 ID + 当前句柄代号。
    Identified(crate::mcp::client::AcpConnectionIdentity),
    /// 是 ACP 连接但身份当前不可证明（归属与声明不一致 / 句柄已换代但未登记 /
    /// 已断连）⇒ 持久化 cache 一律拒绝。
    Unidentifiable,
}

impl McpClientPool {
    pub(crate) async fn configure_peer_cache(&self, peer: &Peer<RoleClient>) {
        if self.cache_policy.get() != Some(&McpCachePolicy::Enabled)
            || self.workspace_scope.get().is_none()
        {
            peer.set_response_cache_config(rmcp::service::ClientCacheConfig::disabled())
                .await;
        }
    }

    /// 当前可证明的 ACP 连接身份。
    ///
    /// 判定面只认池内事实：归属（`acp_owners`）与声明（`acp_connections`）必须
    /// 一致，且句柄必须仍持有本代代号（`advance_handle_generation` 在提交时写入）
    /// ——三者缺一即 fail-closed。
    fn acp_cache_scope(&self, server_name: &str) -> AcpCacheScope {
        let declaration = self.acp_connections.read().get(server_name).cloned();
        let owner = self.acp_owners.read().get(server_name).cloned();
        let declaration = match (declaration, owner) {
            (None, None) => return AcpCacheScope::NotAcp,
            (Some(declaration), Some(owner)) if declaration.session_id == owner => declaration,
            // 只有一半、或归属与声明不一致：不猜身份。
            _ => return AcpCacheScope::Unidentifiable,
        };
        let generation = {
            let clients = self.clients.read();
            let Some(handle) = clients.get(server_name) else {
                return AcpCacheScope::Unidentifiable;
            };
            self.handle_generation(handle)
        };
        // 0 = 本代代号未登记（未提交 / 已换代未登记）：不视为可复用身份。
        if generation == 0 {
            return AcpCacheScope::Unidentifiable;
        }
        AcpCacheScope::Identified(crate::mcp::client::AcpConnectionIdentity {
            session_id: declaration.session_id,
            connection_id: declaration.connection_id,
            generation,
        })
    }

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

    /// 该 server 当前可用的连接身份键。
    ///
    /// `None` = 会话级 ACP 连接但身份当前不可证明 ⇒ 持久化 cache 一律拒绝
    /// （M7 fail-closed；不猜身份、不跨会话按同名 server 命中）。
    fn connection_key_for(&self, server_name: &str) -> Option<McpConnectionKey> {
        match self.acp_cache_scope(server_name) {
            AcpCacheScope::NotAcp => Some(McpConnectionKey::static_server(server_name)),
            AcpCacheScope::Identified(identity) => {
                Some(McpConnectionKey::acp(server_name, identity))
            }
            AcpCacheScope::Unidentifiable => None,
        }
    }

    pub(crate) fn persistent_cache_allowed(&self, server_name: &str) -> bool {
        self.connection_key_for(server_name)
            .is_some_and(|connection| self.persistent_cache_allowed_for(&connection))
    }

    pub(crate) fn persistent_cache_allowed_for(&self, connection: &McpConnectionKey) -> bool {
        self.cache_denial(connection).is_none()
    }

    fn cache_denial(&self, connection: &McpConnectionKey) -> Option<CacheDenial> {
        if self.workspace_scope.get().is_none() {
            return Some(CacheDenial::WorkspaceUnbound);
        }
        match self.cache_policy.get() {
            None => return Some(CacheDenial::Pending),
            Some(McpCachePolicy::Disabled) => return Some(CacheDenial::Configuration),
            Some(McpCachePolicy::Enabled) => {}
        }
        if connection.is_dynamic() {
            // Dynamic MCP 首版 fail-closed：在 scoped cache ticket/current-instance
            // fencing 完成前，不读取或写入持久化 resource cache。
            return Some(CacheDenial::DynamicConnection);
        }
        if let Some(identity) = connection.acp_identity() {
            // 同代命中才可复用：换代（generation）/ 更换声明（session / connection）
            // 或身份不可证明都拒绝——不跨会话按同名 server 命中（M7）。
            return match self.acp_cache_scope(connection.server_name()) {
                AcpCacheScope::Identified(current) if current == *identity => None,
                _ => Some(CacheDenial::AcpConnection),
            };
        }
        let allowed = self
            .configs
            .read()
            .get(connection.server_name())
            .is_none_or(Self::config_allows_persistent_cache);
        (!allowed).then_some(CacheDenial::Credentials)
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
    ) -> Result<(ReadResourceResult, Option<ConnectionCacheTicket>), rmcp::service::ServiceError>
    {
        let connection = self.capture_cache_connection(server_name, peer)?;
        if uri.starts_with("peri-output://") || !self.connection_cache_allowed(&connection) {
            let result = peer
                .read_resource(ReadResourceRequestParams::new(uri))
                .await?;
            self.require_cache_connection_current(&connection)?;
            return Ok((result, None));
        }
        let origin = connection.origin.clone();
        let cache_version = connection.version.clone();
        if let Some(result) = self
            .resource_cache
            .get_versioned(&origin, "resources/read", uri, cache_version.as_deref())
            .await
        {
            self.require_cache_connection_current(&connection)?;
            return Ok((result, None));
        }
        self.resource_cache
            .mark_live_fetch(&origin, "resources/read");
        let Some(ticket) = self
            .resource_cache
            .ticket(&origin, "resources/read", uri)
            .await
        else {
            let result = peer
                .read_resource(ReadResourceRequestParams::new(uri))
                .await?;
            self.require_cache_connection_current(&connection)?;
            return Ok((result, None));
        };
        self.require_cache_connection_current(&connection)?;
        let result = peer
            .read_resource(ReadResourceRequestParams::new(uri))
            .await?;
        self.require_cache_connection_current(&connection)?;
        Ok((result, Some(ConnectionCacheTicket { connection, ticket })))
    }

    /// 仅由资源使用方在内容验证成功后调用。这样受 SEP-2640 内容绑定保护的
    /// `skill://` 响应不会在 digest 校验失败时落入跨进程缓存。
    pub(crate) async fn cache_verified_resource(
        &self,
        server_name: &str,
        ticket: Option<ConnectionCacheTicket>,
        result: &ReadResourceResult,
    ) {
        let Some(ticket) = ticket else { return };
        if ticket.connection.name != server_name {
            return;
        }
        self.persist_cacheable_response(
            &ticket.connection,
            &ticket.ticket,
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
        let connection = self.capture_cache_connection(server_name, peer)?;
        self.list_resources_with_connection(&connection, params, peer)
            .await
    }

    async fn list_resources_with_connection(
        &self,
        connection: &CacheConnection,
        params: Option<PaginatedRequestParams>,
        peer: &Peer<RoleClient>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::service::ServiceError> {
        if !self.connection_cache_allowed(connection) {
            let result = peer.list_resources(params).await?;
            self.require_cache_connection_current(connection)?;
            return Ok(result);
        }
        let origin = connection.origin.clone();
        let params_key = serde_json::to_string(&params).unwrap_or_default();
        let cache_version = connection.version.clone();
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
            self.require_cache_connection_current(connection)?;
            return Ok(result);
        }
        self.resource_cache
            .mark_live_fetch(&origin, "resources/list");
        let ticket = self
            .resource_cache
            .ticket(&origin, "resources/list", &params_key)
            .await;
        self.require_cache_connection_current(connection)?;
        let result = peer.list_resources(params).await?;
        self.require_cache_connection_current(connection)?;
        if let Some(ticket) = ticket {
            self.persist_cacheable_response(
                connection,
                &ticket,
                &result,
                result.ttl_ms,
                result.cache_scope,
            )
            .await;
        }
        self.require_cache_connection_current(connection)?;
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

    pub(crate) async fn list_all_resources_cached_for_startup(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
        config: &McpServerConfig,
    ) -> Result<Vec<Resource>, rmcp::service::ServiceError> {
        let connection = self.capture_startup_cache_connection(server_name, peer, config)?;
        let mut resources = Vec::new();
        let mut cursor = None;
        loop {
            let result = self
                .list_resources_with_connection(
                    &connection,
                    Some(PaginatedRequestParams::default().with_cursor(cursor)),
                    peer,
                )
                .await?;
            resources.extend(result.resources);
            cursor = result.next_cursor;
            if cursor.is_none() {
                self.require_cache_connection_current(&connection)?;
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
        let connection = self.capture_cache_connection(server_name, peer)?;
        if !self.connection_cache_allowed(&connection) {
            let result = peer.list_resource_templates(params).await?;
            self.require_cache_connection_current(&connection)?;
            return Ok(result);
        }
        let origin = connection.origin.clone();
        let params_key = serde_json::to_string(&params).unwrap_or_default();
        let cache_version = connection.version.clone();
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
            self.require_cache_connection_current(&connection)?;
            return Ok(result);
        }
        let ticket = self
            .resource_cache
            .ticket(&origin, "resources/templates/list", &params_key)
            .await;
        self.require_cache_connection_current(&connection)?;
        let result = peer.list_resource_templates(params).await?;
        self.require_cache_connection_current(&connection)?;
        if let Some(ticket) = ticket {
            self.persist_cacheable_response(
                &connection,
                &ticket,
                &result,
                result.ttl_ms,
                result.cache_scope,
            )
            .await;
        }
        self.require_cache_connection_current(&connection)?;
        Ok(result)
    }

    pub(crate) async fn invalidate_resource_cache(&self, server_name: &str, uri: Option<&str>) {
        if self.workspace_scope.get().is_none() {
            return;
        }
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
        let connection = self.capture_cache_connection(server_name, peer)?;
        self.list_all_tools_with_connection(&connection, peer).await
    }

    pub(crate) async fn list_all_tools_cached_for_startup(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
        config: &McpServerConfig,
    ) -> Result<Vec<Tool>, rmcp::service::ServiceError> {
        let connection = self.capture_startup_cache_connection(server_name, peer, config)?;
        self.list_all_tools_with_connection(&connection, peer).await
    }

    async fn list_all_tools_with_connection(
        &self,
        connection: &CacheConnection,
        peer: &Peer<RoleClient>,
    ) -> Result<Vec<Tool>, rmcp::service::ServiceError> {
        if !self.connection_cache_allowed(connection)
            || !self.tools_cache_eligible(&connection.name)
        {
            let result = peer.list_all_tools().await?;
            self.require_cache_connection_current(connection)?;
            return Ok(result);
        }
        let origin = connection.origin.clone();
        let cache_version = connection.version.clone();
        if let Some(version) = cache_version.as_deref() {
            if let Some(tools) = self
                .resource_cache
                .get_versioned::<Vec<Tool>>(&origin, "tools/list", "", Some(version))
                .await
            {
                self.require_cache_connection_current(connection)?;
                return Ok(tools);
            }
        }
        self.resource_cache.mark_live_fetch(&origin, "tools/list");
        let ticket = self.resource_cache.ticket(&origin, "tools/list", "").await;
        self.require_cache_connection_current(connection)?;
        let tools = peer.list_all_tools().await?;
        self.require_cache_connection_current(connection)?;
        if let (Some(ticket), Some(version)) = (ticket, cache_version.as_deref()) {
            if self.connection_cache_allowed(connection) {
                self.resource_cache
                    .put_ticket_versioned(&ticket, std::time::Duration::ZERO, Some(version), &tools)
                    .await;
                if !self.connection_cache_allowed(connection) {
                    self.resource_cache
                        .invalidate(&connection.origin, "tools/list", Some(""))
                        .await;
                }
            }
        }
        self.require_cache_connection_current(connection)?;
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
        if self.workspace_scope.get().is_none() {
            return;
        }
        let origin = self.cache_origin(server_name);
        self.resource_cache
            .invalidate(&origin, "tools/list", None)
            .await;
    }

    pub(crate) async fn invalidate_resource_cache_origin(&self, origin: &str, uri: Option<&str>) {
        if self.workspace_scope.get().is_none() {
            return;
        }
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

    /// 持久化 cache origin（含 workspace 归属）。
    ///
    /// M7：会话级 ACP 连接**不用** transport 身份（同名 server 在不同会话、
    /// 不同连接代下会撞同一 origin），而用「声明会话 + 连接 ID + 连接代」——
    /// 换代 / 换会话即自然 miss；身份不可证明时给一个不可复用的占位 origin
    /// （准入侧同时按 fail-closed 拒绝，两处不互相依赖）。
    /// 身份段只含 opaque 值，凭据（URL query / header / env）不参与。
    pub(crate) fn cache_origin(&self, server_name: &str) -> String {
        let origin = match self.acp_cache_scope(server_name) {
            AcpCacheScope::Identified(identity) => {
                format!("{server_name}\0{}", identity.cache_origin_segment())
            }
            AcpCacheScope::Unidentifiable => {
                format!("{server_name}\0acp-unidentified")
            }
            AcpCacheScope::NotAcp => {
                let config = self.configs.read().get(server_name).cloned();
                crate::mcp::resource_cache::cache_origin(server_name, config.as_ref())
            }
        };
        match self.workspace_scope.get() {
            Some(workspace_id) => format!("{workspace_id}:{origin}"),
            None => format!("unbound:{origin}"),
        }
    }

    pub(super) fn cache_status_for(&self, server_name: &str) -> Option<String> {
        let denial = match self.connection_key_for(server_name) {
            Some(connection) => self.cache_denial(&connection),
            None => Some(CacheDenial::AcpConnection),
        };
        if let Some(reason) = denial {
            return Some(
                match reason {
                    CacheDenial::Pending => "cache_pending",
                    CacheDenial::WorkspaceUnbound => "cache_disabled_workspace_unbound",
                    CacheDenial::Configuration => "cache_disabled_by_config",
                    CacheDenial::DynamicConnection => "cache_disabled_dynamic",
                    CacheDenial::AcpConnection => "cache_disabled_acp_connection",
                    CacheDenial::Credentials => "cache_disabled",
                }
                .to_string(),
            );
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
        Some("cache_ready".to_string())
    }

    async fn persist_cacheable_response<T: serde::Serialize>(
        &self,
        connection: &CacheConnection,
        ticket: &crate::mcp::resource_cache::CacheTicket,
        result: &T,
        ttl_ms: Option<u64>,
        cache_scope: Option<CacheScope>,
    ) {
        if ticket.origin != connection.origin || !self.connection_cache_allowed(connection) {
            return;
        }
        let cache_version = &connection.version;
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
            if !self.connection_cache_allowed(connection) {
                self.resource_cache
                    .invalidate(&ticket.origin, ticket.method(), Some(ticket.params()))
                    .await;
            }
        }
    }
}
