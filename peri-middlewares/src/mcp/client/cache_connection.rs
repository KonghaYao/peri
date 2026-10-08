use std::sync::Arc;

use rmcp::{service::ServiceError, Peer, RoleClient};
use serde::{de::DeserializeOwned, Serialize};

use super::super::{McpClientHandle, McpClientPool, McpConnectionKey};
use crate::mcp::resource_cache::CacheTicket;

#[derive(Clone)]
pub(crate) struct CacheConnection {
    pub(super) name: String,
    handle: Option<Arc<McpClientHandle>>,
    generation: u64,
    key: Option<McpConnectionKey>,
    pub(super) origin: String,
    pub(super) version: Option<String>,
    startup: Option<StartupAdmission>,
}

#[derive(Clone)]
struct StartupAdmission {
    peer: Peer<RoleClient>,
    info: Arc<rmcp::model::ServerPeerInfo>,
    configuration: serde_json::Value,
}

pub(crate) struct ConnectionCacheTicket {
    pub(super) connection: CacheConnection,
    pub(super) ticket: CacheTicket,
}

#[derive(Clone)]
pub(crate) struct ConnectionResourceCache {
    pool: Arc<McpClientPool>,
    connection: CacheConnection,
}

impl McpClientPool {
    pub(super) fn capture_cache_connection(
        &self,
        name: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<CacheConnection, ServiceError> {
        let handle = self.get_client(name);
        if let Some(handle) = &handle {
            let matches = handle.peer.as_ref().is_some_and(|current| {
                std::ptr::eq(current, peer)
                    || current
                        .peer_info()
                        .zip(peer.peer_info())
                        .is_some_and(|(current, captured)| Arc::ptr_eq(&current, &captured))
            });
            if !matches {
                return Err(ServiceError::TransportClosed);
            }
        } else if !matches!(self.acp_cache_scope(name), super::AcpCacheScope::NotAcp) {
            return Err(ServiceError::TransportClosed);
        }
        let connection = CacheConnection {
            name: name.to_owned(),
            generation: handle
                .as_ref()
                .map_or(0, |handle| self.handle_generation(handle)),
            handle,
            key: self.connection_key_for(name),
            origin: self.cache_origin(name),
            version: self.cache_versions.read().get(name).cloned(),
            startup: None,
        };
        self.require_cache_connection_current(&connection)?;
        Ok(connection)
    }

    pub(crate) fn capture_startup_cache_connection(
        &self,
        name: &str,
        peer: &Peer<RoleClient>,
        config: &crate::mcp::config::McpServerConfig,
    ) -> Result<CacheConnection, ServiceError> {
        let configuration =
            serde_json::to_value(config).map_err(|_| ServiceError::UnexpectedResponse)?;
        let admitted = self
            .configs
            .read()
            .get(name)
            .and_then(|config| serde_json::to_value(config).ok());
        let info = peer.peer_info().ok_or(ServiceError::TransportClosed)?;
        if admitted.as_ref() != Some(&configuration)
            || config.disabled == Some(true)
            || matches!(config.source, Some(crate::mcp::config::ConfigSource::Acp))
            || !matches!(self.acp_cache_scope(name), super::AcpCacheScope::NotAcp)
        {
            return Err(ServiceError::TransportClosed);
        }
        let handle = self.get_client(name);
        let connection = CacheConnection {
            name: name.to_owned(),
            generation: handle
                .as_ref()
                .map_or(0, |handle| self.handle_generation(handle)),
            handle,
            key: Some(McpConnectionKey::static_server(name)),
            origin: self.cache_origin(name),
            version: super::super::service::peer_cache_version(peer),
            startup: Some(StartupAdmission {
                peer: peer.clone(),
                info,
                configuration,
            }),
        };
        self.require_cache_connection_current(&connection)?;
        Ok(connection)
    }

    pub(crate) fn require_cache_connection_current(
        &self,
        connection: &CacheConnection,
    ) -> Result<(), ServiceError> {
        let current = self.get_client(&connection.name);
        let same_handle = match (&connection.handle, current) {
            (Some(captured), Some(current)) => {
                Arc::ptr_eq(captured, &current)
                    && self.handle_generation(&current) == connection.generation
            }
            (None, None) => true,
            _ => false,
        };
        let startup_current = connection.startup.as_ref().is_none_or(|admission| {
            let configuration = self
                .configs
                .read()
                .get(&connection.name)
                .and_then(|config| serde_json::to_value(config).ok());
            !admission.peer.is_transport_closed()
                && admission
                    .peer
                    .peer_info()
                    .is_some_and(|info| Arc::ptr_eq(&info, &admission.info))
                && configuration.as_ref() == Some(&admission.configuration)
                && super::super::service::peer_cache_version(&admission.peer) == connection.version
        });
        if self.is_open()
            && same_handle
            && startup_current
            && self.connection_key_for(&connection.name) == connection.key
        {
            Ok(())
        } else {
            tracing::warn!(server = %connection.name, "MCP cache request connection changed");
            Err(ServiceError::TransportClosed)
        }
    }

    pub(super) fn connection_cache_allowed(&self, connection: &CacheConnection) -> bool {
        (connection.handle.is_some() || connection.startup.is_some())
            && self.require_cache_connection_current(connection).is_ok()
            && connection
                .key
                .as_ref()
                .is_some_and(|key| self.persistent_cache_allowed_for(key))
            && self.cache_origin(&connection.name) == connection.origin
            && self.cache_versions.read().get(&connection.name).cloned() == connection.version
    }

    pub(crate) fn resource_cache_for_handle(
        self: &Arc<Self>,
        handle: &Arc<McpClientHandle>,
    ) -> Option<(ConnectionResourceCache, String)> {
        let peer = handle.peer.as_ref()?;
        let connection = self.capture_cache_connection(&handle.name, peer).ok()?;
        if !connection
            .handle
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, handle))
        {
            return None;
        }
        let origin = connection.origin.clone();
        Some((
            ConnectionResourceCache {
                pool: self.clone(),
                connection,
            },
            origin,
        ))
    }
}

impl ConnectionResourceCache {
    pub(crate) fn is_current(&self) -> bool {
        self.pool
            .require_cache_connection_current(&self.connection)
            .is_ok()
    }

    pub(crate) fn mark_live_fetch(&self, origin: &str, method: &'static str) {
        if origin == self.connection.origin && self.pool.connection_cache_allowed(&self.connection)
        {
            self.pool.resource_cache.mark_live_fetch(origin, method);
        }
    }

    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> Option<T> {
        if origin != self.connection.origin || !self.pool.connection_cache_allowed(&self.connection)
        {
            return None;
        }
        let result = self
            .pool
            .resource_cache
            .get_versioned(origin, method, params, self.connection.version.as_deref())
            .await;
        self.pool
            .connection_cache_allowed(&self.connection)
            .then_some(result)
            .flatten()
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> Option<T> {
        self.get(origin, method, params).await
    }

    pub(crate) async fn ticket(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> Option<CacheTicket> {
        if origin != self.connection.origin || !self.pool.connection_cache_allowed(&self.connection)
        {
            return None;
        }
        let ticket = self
            .pool
            .resource_cache
            .ticket(origin, method, params)
            .await;
        self.pool
            .connection_cache_allowed(&self.connection)
            .then_some(ticket)
            .flatten()
    }

    pub(crate) async fn put_ticket<T: Serialize>(
        &self,
        ticket: &CacheTicket,
        ttl: std::time::Duration,
        result: &T,
    ) {
        if ticket.origin != self.connection.origin
            || !self.pool.connection_cache_allowed(&self.connection)
        {
            return;
        }
        self.pool
            .resource_cache
            .put_ticket_versioned(ticket, ttl, self.connection.version.as_deref(), result)
            .await;
        if !self.pool.connection_cache_allowed(&self.connection) {
            self.pool
                .resource_cache
                .invalidate(&ticket.origin, ticket.method(), Some(ticket.params()))
                .await;
        }
    }
}
