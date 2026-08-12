use std::sync::Arc;

use super::{
    auth_store::FileCredentialStore,
    client::{
        build_authed_transport, build_http_transport, spawn_stdio_transport, ClientStatus,
        McpClientHandle, McpClientPool, McpPoolError, McpServiceWrapper, OAuthStatus,
        HTTP_CONNECT_TIMEOUT, SHUTDOWN_TIMEOUT, STDIO_CONNECT_TIMEOUT,
    },
    oauth_flow::{OAuthFlowEvent, OAuthFlowManager},
    transport::TransportConfig,
};

impl McpClientPool {
    pub async fn reconnect(
        self: &Arc<Self>,
        server_name: &str,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
    ) -> Result<(), McpPoolError> {
        let server_config = self
            .configs
            .read()
            .get(server_name)
            .cloned()
            .ok_or_else(|| McpPoolError::NotConnected {
                server: server_name.to_string(),
                status: ClientStatus::Disconnected,
            })?;

        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        // 重连前捕获旧状态：insert 覆盖后由 record_status_change 判定是否
        // 构成上下线变化（Connected→Failed 等）；首次插入（旧状态不存在）
        // 不产生通知（初始化阶段由首 turn 概览覆盖）。
        let old_status = self
            .clients
            .read()
            .get(server_name)
            .map(|c| c.status.clone());
        self.clients.write().remove(server_name);

        let tc = TransportConfig::try_from(&server_config).map_err(|e| {
            McpPoolError::ConnectionFailed {
                server: server_name.to_string(),
                reason: format!("传输层构建失败: {e}"),
            }
        })?;
        let is_http = matches!(tc, TransportConfig::StreamableHttp { .. });
        let timeout = if is_http {
            HTTP_CONNECT_TIMEOUT
        } else {
            STDIO_CONNECT_TIMEOUT
        };

        let mut used_oauth = false;
        let result = match &tc {
            TransportConfig::Stdio { command, args, env } => {
                match spawn_stdio_transport(command, args, env) {
                    Ok(t) => {
                        tokio::time::timeout(timeout, rmcp::service::serve_client((), t)).await
                    }
                    Err(e) => {
                        McpClientPool::insert_failed(self, server_name, format!("stdio 失败: {e}"));
                        return Err(McpPoolError::ConnectionFailed {
                            server: server_name.to_string(),
                            reason: format!("stdio 失败: {e}"),
                        });
                    }
                }
            }
            TransportConfig::StreamableHttp {
                url,
                headers,
                oauth,
            } => {
                // 与 run_initialize 一致：检查磁盘是否有已保存的 OAuth 凭证
                let oauth_cfg = oauth.as_ref().cloned().or_else(|| {
                    let token_store = Arc::new(FileCredentialStore::new());
                    match tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current()
                            .block_on(token_store.load_server(server_name))
                    }) {
                        Ok(Some(_)) => {
                            tracing::info!(server = %server_name, "发现已保存的 OAuth 凭证，使用默认配置恢复");
                            Some(super::config::OAuthConfig::default())
                        }
                        _ => None,
                    }
                });
                if let Some(cfg) = oauth_cfg {
                    // callback 可空：优先调用方传入，其次 pool 级回调（host
                    // 装配注入，事件转发 TUI popup），都没有则 no-op 静默授权。
                    let cb: Arc<dyn Fn(OAuthFlowEvent) + Send + Sync> = oauth_event_callback
                        .map(Arc::from)
                        .or_else(|| self.oauth_event_callback())
                        .unwrap_or_else(|| Arc::new(|_| {}));
                    let ts = Arc::new(FileCredentialStore::new());
                    let mut mgr = OAuthFlowManager::new_with_arc(ts, cb);
                    match mgr.run_oauth_flow(server_name, url, &cfg).await {
                        Ok(()) => {
                            used_oauth = true;
                            if let Some(am) = mgr.get_authorization_manager(server_name) {
                                tokio::time::timeout(
                                    timeout,
                                    rmcp::service::serve_client(
                                        (),
                                        build_authed_transport(url, headers, am),
                                    ),
                                )
                                .await
                            } else {
                                tokio::time::timeout(
                                    timeout,
                                    rmcp::service::serve_client(
                                        (),
                                        build_http_transport(url, headers),
                                    ),
                                )
                                .await
                            }
                        }
                        Err(e) => {
                            tracing::warn!(server = %server_name, error = %e, "OAuth 恢复失败，尝试裸连接");
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client((), build_http_transport(url, headers)),
                            )
                            .await
                        }
                    }
                } else {
                    tokio::time::timeout(
                        timeout,
                        rmcp::service::serve_client((), build_http_transport(url, headers)),
                    )
                    .await
                }
            }
        };

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
                let oauth_status = if used_oauth {
                    OAuthStatus::Authorized
                } else {
                    OAuthStatus::default()
                };
                self.clients.write().insert(
                    server_name.to_string(),
                    Arc::new(McpClientHandle {
                        name: server_name.to_string(),
                        peer: Some(peer),
                        tools,
                        resources,
                        status: ClientStatus::Connected,
                        oauth_status,
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable: false,
                    }),
                );
                self.services
                    .lock()
                    .await
                    .insert(server_name.to_string(), McpServiceWrapper::Default(rs));
                self.record_status_change(server_name, old_status.as_ref());
                Ok(())
            }
            Ok(Err(e)) => {
                let err_str = e.to_string();
                if McpClientPool::is_auth_required_error(&err_str, is_http) {
                    McpClientPool::insert_needs_auth(self, server_name, err_str.clone());
                } else {
                    McpClientPool::insert_failed(self, server_name, err_str.clone());
                }
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: err_str,
                })
            }
            Err(_) => {
                let msg = "连接超时";
                McpClientPool::insert_failed(self, server_name, msg.to_string());
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: msg.to_string(),
                })
            }
        }
    }
}
