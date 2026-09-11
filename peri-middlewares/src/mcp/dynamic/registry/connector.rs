//! 生产连接准备与 SecretResolver 装配；连接资源仍由 staged connection 持有。

use super::super::staged_connection::{
    prepare_single_server, EnvironmentSecretResolver, RejectingSecretResolver, StagedMcpConnection,
};
use crate::mcp::{task_scope::McpTaskSpawner, McpClientPool};
use async_trait::async_trait;
use peri_acp_types::{
    dynamic_mcp::{
        CanonicalDynamicMcpConfig, DynamicMcpFailure, DynamicMcpInstanceKey, DynamicMcpOperationId,
        DynamicMcpOperationState,
    },
    ports::SecretResolverPort,
};
use std::sync::Arc;

#[async_trait]
pub trait DynamicMcpConnector: Send + Sync {
    async fn prepare(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: DynamicMcpOperationId,
        config: CanonicalDynamicMcpConfig,
        progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
    ) -> Result<StagedMcpConnection, DynamicMcpFailure>;
}

pub struct ProductionDynamicMcpConnector {
    secret_resolver: Arc<dyn SecretResolverPort>,
    cleanup_spawner: McpTaskSpawner,
    oauth_pool: Arc<McpClientPool>,
}

impl ProductionDynamicMcpConnector {
    pub fn new(
        secret_resolver: Arc<dyn SecretResolverPort>,
        cleanup_spawner: McpTaskSpawner,
        oauth_pool: Arc<McpClientPool>,
    ) -> Self {
        Self {
            secret_resolver,
            cleanup_spawner,
            oauth_pool,
        }
    }

    pub fn from_environment(
        cleanup_spawner: McpTaskSpawner,
        oauth_pool: Arc<McpClientPool>,
    ) -> Self {
        Self::new(
            Arc::new(EnvironmentSecretResolver),
            cleanup_spawner,
            oauth_pool,
        )
    }

    pub fn fail_closed(cleanup_spawner: McpTaskSpawner, oauth_pool: Arc<McpClientPool>) -> Self {
        Self::new(
            Arc::new(RejectingSecretResolver),
            cleanup_spawner,
            oauth_pool,
        )
    }
}

#[async_trait]
impl DynamicMcpConnector for ProductionDynamicMcpConnector {
    async fn prepare(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: DynamicMcpOperationId,
        config: CanonicalDynamicMcpConfig,
        progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
    ) -> Result<StagedMcpConnection, DynamicMcpFailure> {
        prepare_single_server(
            instance,
            flow_id,
            &config,
            self.secret_resolver.as_ref(),
            self.cleanup_spawner.clone(),
            Arc::clone(&self.oauth_pool),
            progress,
        )
        .await
    }
}
