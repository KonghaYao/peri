use std::{path::Path, sync::Arc, time::Duration};

use rmcp::{service::RunningService, ClientLifecycleMode, RoleClient};

use crate::mcp::{
    client::McpClientHandle, config::ConfigSource, ClientStatus, McpClientPool, OAuthStatus,
};

pub(super) struct ImageFixture {
    pub pool: Arc<McpClientPool>,
    service: RunningService<RoleClient, ()>,
    server: tokio::task::JoinHandle<Result<rmcp::service::QuitReason, tokio::task::JoinError>>,
}

impl ImageFixture {
    pub async fn new(cwd: &Path) -> Self {
        let handler = peri_mcp_workspace::WorkspaceMcpServer::new(cwd.to_string_lossy(), None);
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            rmcp::serve_server(handler, tokio::io::split(server_io))
                .await
                .unwrap()
                .waiting()
                .await
        });
        let service = tokio::time::timeout(
            Duration::from_secs(2),
            rmcp::serve_client_with_lifecycle(
                (),
                tokio::io::split(client_io),
                ClientLifecycleMode::Auto {
                    preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
                    legacy_version: None,
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let pool = Arc::new(McpClientPool::new_empty());
        pool.clients.write().insert(
            "workspace".to_owned(),
            Arc::new(McpClientHandle {
                name: "workspace".to_owned(),
                version: None,
                connected_at: None,
                protocol_version: None,
                cache_version: None,
                peer: Some(service.peer().clone()),
                tools: Vec::new(),
                resources: Vec::new(),
                status: ClientStatus::Connected,
                oauth_status: OAuthStatus::None,
                source: Some(ConfigSource::Builtin {
                    instance: "workspace".to_owned(),
                }),
                url: None,
                skills_capable: false,
            }),
        );
        Self {
            pool,
            service,
            server,
        }
    }

    pub fn middleware(&self) -> super::ImageMiddleware {
        super::ImageMiddleware::new().with_mcp_pool(
            self.pool.clone(),
            "image-session".to_owned(),
            &Default::default(),
        )
    }

    pub async fn shutdown(mut self) {
        self.pool.clients.write().clear();
        let _ = self
            .service
            .close_with_timeout(Duration::from_millis(500))
            .await;
        if tokio::time::timeout(Duration::from_millis(500), &mut self.server)
            .await
            .is_err()
        {
            self.server.abort();
            let _ = self.server.await;
        }
    }
}
