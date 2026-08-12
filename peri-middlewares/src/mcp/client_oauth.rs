use std::sync::Arc;

use super::{
    auth_store::FileCredentialStore,
    client::{
        build_authed_transport, ClientStatus, McpClientHandle, McpClientPool, McpPoolError,
        McpServiceWrapper, OAuthStatus, HTTP_CONNECT_TIMEOUT, SHUTDOWN_TIMEOUT,
    },
    oauth_flow::{OAuthFlowEvent, OAuthFlowManager},
};

impl McpClientPool {
    pub async fn start_oauth_flow(
        self: &Arc<Self>,
        server_name: &str,
        oauth_event_callback: Box<dyn Fn(OAuthFlowEvent) + Send + Sync>,
    ) -> Result<(), McpPoolError> {
        let cfg = self
            .configs
            .read()
            .get(server_name)
            .cloned()
            .ok_or_else(|| McpPoolError::NotConnected {
                server: server_name.to_string(),
                status: ClientStatus::Disconnected,
            })?;
        let url = cfg.url.as_deref().unwrap_or("").to_string();
        // 使用显式 OAuth 配置，或对 HTTP 服务器回退到默认配置（启用 DCR 自动发现）
        let oauth_cfg = match cfg.oauth.as_ref().filter(|o| o.is_enabled()) {
            Some(explicit) => explicit.clone(),
            None => {
                if cfg.url.is_none() {
                    return Err(McpPoolError::ConnectionFailed {
                        server: server_name.to_string(),
                        reason: "仅 HTTP 传输支持 OAuth".to_string(),
                    });
                }
                super::config::OAuthConfig::default()
            }
        };
        let ts = Arc::new(FileCredentialStore::new());
        let mut mgr = OAuthFlowManager::new(ts, oauth_event_callback);
        mgr.run_oauth_flow(server_name, &url, &oauth_cfg)
            .await
            .map_err(|e| McpPoolError::ConnectionFailed {
                server: server_name.to_string(),
                reason: format!("OAuth 授权失败: {e}"),
            })?;

        // 从 OAuth 流程中提取 AuthorizationManager，用于构建认证传输层
        let auth_manager = mgr.get_authorization_manager(server_name).ok_or_else(|| {
            McpPoolError::ConnectionFailed {
                server: server_name.to_string(),
                reason: "OAuth 授权完成但无法提取 AuthorizationManager".to_string(),
            }
        })?;

        // 关闭旧连接
        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        let old_status = self
            .clients
            .read()
            .get(server_name)
            .map(|c| c.status.clone());
        self.clients.write().remove(server_name);

        // 使用认证传输层重新连接
        let headers = cfg.headers.clone().unwrap_or_default();
        let result = tokio::time::timeout(
            HTTP_CONNECT_TIMEOUT,
            rmcp::service::serve_client((), build_authed_transport(&url, &headers, auth_manager)),
        )
        .await;

        match result {
            Ok(Ok(rs)) => {
                let tools =
                    rs.list_all_tools()
                        .await
                        .map_err(|e| McpPoolError::ToolDiscoveryFailed {
                            server: server_name.to_string(),
                            reason: e.to_string(),
                        })?;
                let resources = rs.list_all_resources().await.unwrap_or_default();
                let peer = rs.peer().clone();
                let handle = Arc::new(McpClientHandle {
                    name: server_name.to_string(),
                    peer: Some(peer),
                    tools,
                    resources,
                    status: ClientStatus::Connected,
                    oauth_status: OAuthStatus::Authorized,
                    source: cfg.source.clone(),
                    url: cfg.url.clone(),
                    channel_capable: false,
                });
                self.clients.write().insert(server_name.to_string(), handle);
                self.services
                    .lock()
                    .await
                    .insert(server_name.to_string(), McpServiceWrapper::Default(rs));
                self.record_status_change(server_name, old_status.as_ref());
                Ok(())
            }
            Ok(Err(e)) => {
                let err_str = e.to_string();
                if Self::is_auth_required_error(&err_str, true) {
                    Self::insert_needs_auth(self, server_name, err_str.clone());
                } else {
                    Self::insert_failed(self, server_name, err_str.clone());
                }
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: err_str,
                })
            }
            Err(_) => {
                let msg = "连接超时".to_string();
                Self::insert_failed(self, server_name, msg.clone());
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: msg,
                })
            }
        }
    }

    /// 清除指定服务器的 OAuth 凭证并断开连接
    pub async fn clear_oauth(self: &Arc<Self>, server_name: &str) -> Result<(), McpPoolError> {
        // 1. 清除 token 文件中的凭证
        let store = FileCredentialStore::new();
        let _ = store.clear_server(server_name).await;

        // 2. 关闭连接
        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }

        // 3. 更新 handle 为 NeedsAuthorization
        let old_status = self
            .clients
            .read()
            .get(server_name)
            .map(|c| c.status.clone());
        let (source, url) = self
            .configs
            .read()
            .get(server_name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        self.clients.write().insert(
            server_name.to_string(),
            Arc::new(McpClientHandle {
                name: server_name.to_string(),
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed("OAuth credentials cleared".to_string()),
                oauth_status: OAuthStatus::NeedsAuthorization,
                source,
                url,
                channel_capable: false,
            }),
        );
        self.record_status_change(server_name, old_status.as_ref());

        Ok(())
    }
}
