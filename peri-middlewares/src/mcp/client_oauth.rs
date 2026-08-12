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
    /// 异步触发 OAuth 授权流程（不阻塞调用方）。
    ///
    /// 授权任务独立 spawn：服务器先标记 `NeedsAuthorization`，`run_oauth_flow`
    /// 期间的事件（`AuthorizationNeeded` / `Completed` / `Failed`）经装配面
    /// 注入的 `oauth_event_callback` 转发给 TUI；授权成功后自动用
    /// `AuthorizationManager` 重建认证传输层并连接。
    ///
    /// 无 `oauth_event_callback`（TUI 面板池，UI 无法弹 popup）时降级为
    /// 快速路径：仅尝试恢复磁盘凭证连接，不启动完整授权（不弹窗、不阻塞）；
    /// 凭据缺失/失效时保持 `NeedsAuthorization`，由 host pool 授权完成后
    /// 各 pool 经共享 `FileCredentialStore` 恢复。
    pub fn spawn_oauth_flow(self: &Arc<Self>, server_name: &str) {
        let pool = self.clone();
        let server_name = server_name.to_string();
        tokio::spawn(async move {
            if pool.oauth_event_callback().is_none() {
                let _ = pool.start_oauth_flow(&server_name, true).await;
                return;
            }
            Self::insert_needs_auth(&pool, &server_name, "OAuth 授权进行中".to_string());
            match pool.start_oauth_flow(&server_name, false).await {
                Ok(()) => {}
                Err(e) => {
                    // run_oauth_flow 内部已在取消/超时路径发 AuthorizationFailed；
                    // 其余错误（AuthError / callback 服务器错误）在此补发，保证
                    // TUI 收到失败通知并保持 NeedsAuthorization 状态。
                    if let Some(cb) = pool.oauth_event_callback() {
                        cb(OAuthFlowEvent::AuthorizationFailed {
                            server_name: server_name.clone(),
                            error: e.to_string(),
                        });
                    }
                }
            }
        });
    }

    /// 执行 OAuth 授权流程（异步，两轮尝试）。
    ///
    /// `quick_only=true`（面板池路径）：只跑第一轮「恢复磁盘凭证 → 连接」，
    /// 凭据失效时不清除、不启动完整授权，直接返回错误（保持 NeedsAuthorization）。
    /// `quick_only=false`（host pool 路径）：第一轮恢复失败/凭据失效时清除
    /// 失效凭证，第二轮走完整授权（DCR + PKCE + AuthorizationNeeded 弹 popup）。
    pub async fn start_oauth_flow(
        self: &Arc<Self>,
        server_name: &str,
        quick_only: bool,
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
        let event_cb = self
            .oauth_event_callback()
            .unwrap_or_else(|| Arc::new(|_| {}) as Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>);

        // 两轮尝试：第一轮优先恢复磁盘凭证（可能已过期/被 revoke）；恢复后
        // 连接仍要求授权（401）时清除失效凭证，第二轮走完整授权流程（弹
        // popup 让用户重新授权）。第二轮再失败直接返回错误。
        // quick_only（面板池路径）只跑第一轮：401 时不清除凭据、不完整授权。
        let rounds: u8 = if quick_only { 1 } else { 2 };
        for attempt in 0..rounds {
            let mut mgr = OAuthFlowManager::new_with_arc(ts.clone(), event_cb.clone());
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
                rmcp::service::serve_client(
                    (),
                    build_authed_transport(&url, &headers, auth_manager),
                ),
            )
            .await;

            match result {
                Ok(Ok(rs)) => {
                    let tools = rs.list_all_tools().await.map_err(|e| {
                        McpPoolError::ToolDiscoveryFailed {
                            server: server_name.to_string(),
                            reason: e.to_string(),
                        }
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
                    return Ok(());
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    if Self::is_auth_required_error(&err_str, true) {
                        if attempt == 0 && !quick_only {
                            // 磁盘凭证已失效（过期/被服务端 revoke）：清除后
                            // 第二轮走完整授权（弹 popup），保证用户可重新授权。
                            tracing::info!(server = %server_name, "恢复的 OAuth 凭证已失效，清除并重新授权");
                            let _ = ts.clear_server(server_name).await;
                            continue;
                        }
                        Self::insert_needs_auth(self, server_name, err_str.clone());
                    } else {
                        Self::insert_failed(self, server_name, err_str.clone());
                    }
                    return Err(McpPoolError::ConnectionFailed {
                        server: server_name.to_string(),
                        reason: err_str,
                    });
                }
                Err(_) => {
                    let msg = "连接超时".to_string();
                    Self::insert_failed(self, server_name, msg.clone());
                    return Err(McpPoolError::ConnectionFailed {
                        server: server_name.to_string(),
                        reason: msg,
                    });
                }
            }
        }
        unreachable!("start_oauth_flow 循环内必返回")
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
