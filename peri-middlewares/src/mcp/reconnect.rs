use std::sync::Arc;

use super::{
    auth_store::FileCredentialStore,
    client::{
        build_authed_transport, build_http_transport, serve_client_auto, setup_subscription,
        ClientStatus, McpClientHandle, McpClientPool, McpPoolError, OAuthStartDisposition,
        OAuthStatus, HTTP_CONNECT_TIMEOUT, SHUTDOWN_TIMEOUT, STDIO_CONNECT_TIMEOUT,
    },
    initialize::{
        commit_discovery_failure, commit_discovery_success, downgrade_resource_listing,
        fail_tool_discovery, list_discovered_tools,
    },
    oauth_flow::{OAuthFlowEvent, OAuthFlowManager},
    transport::TransportConfig,
};

impl McpClientPool {
    pub fn spawn_reconnect(
        self: &Arc<Self>,
        server_name: String,
    ) -> Result<(), super::task_scope::McpTaskScopeClosed> {
        let pool = Arc::clone(self);
        let key = super::task_scope::McpTaskKey::Reconnect(server_name.clone());
        self.spawn_background(key, async move {
            if let Err(error) = pool.reconnect(&server_name, None).await {
                tracing::warn!(server = %server_name, error = %error, "MCP reconnect failed");
            }
        })
    }

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

        // 重新发现开始：旧代证据立即作废，等待方按「仍在进行」重新判定
        // （旧代证据即使保留也不会被接受，但显式清除让等待方立刻重读事实）。
        self.clear_discovery_evidence(server_name);
        // Stop and join the old keyed subscription outside pool locks before
        // replacing its service, so a cancelled caller cannot detach it.
        self.stop_background(&super::task_scope::McpTaskKey::Subscription(
            server_name.to_string(),
        ))
        .await;
        let previous_service = { self.services.lock().remove(server_name) };
        if let Some(mut svc) = previous_service {
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
        // lifecycle 仅由显式 protocolVersion 选择；subscriptions 只负责连接后订阅。
        let protocol_version = server_config.protocol_version.as_ref();
        let subscriptions = server_config
            .subscriptions
            .as_ref()
            .filter(|s| !s.is_empty());

        let mut used_oauth = false;
        let result = match &tc {
            TransportConfig::Stdio { command, args, env } => {
                let cwd =
                    self.execution_cwd
                        .get()
                        .ok_or_else(|| McpPoolError::ConnectionFailed {
                            server: server_name.to_owned(),
                            reason: "MCP execution directory is not initialized".into(),
                        })?;
                match self.spawn_stdio_transport(command, args, env, cwd) {
                    Ok(t) => {
                        serve_client_auto(
                            t,
                            None,
                            protocol_version,
                            &self.capability_profile,
                            timeout,
                        )
                        .await
                    }
                    Err(e) => {
                        McpClientPool::insert_failed(self, server_name, format!("stdio 失败: {e}"));
                        commit_discovery_failure(self, server_name, false);
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
                    let flow_id = uuid::Uuid::now_v7().to_string();
                    match self.reserve_oauth_flow(server_name, &flow_id) {
                        OAuthStartDisposition::Started => {}
                        OAuthStartDisposition::AlreadyActive
                        | OAuthStartDisposition::Conflict { .. } => {
                            return Err(McpPoolError::ConnectionFailed {
                                server: server_name.to_string(),
                                reason: "OAuth authorization already active".to_string(),
                            });
                        }
                    }
                    // callback 可空：优先调用方传入，其次 pool 级回调（host
                    // 装配注入，事件转发 TUI popup），都没有则 no-op 静默授权。
                    let cb: Arc<dyn Fn(OAuthFlowEvent) + Send + Sync> = oauth_event_callback
                        .map(Arc::from)
                        .or_else(|| self.oauth_event_callback())
                        .unwrap_or_else(|| Arc::new(|_| {}));
                    let ts = Arc::new(FileCredentialStore::new());
                    let mut mgr = OAuthFlowManager::new_with_arc(ts, cb);
                    let oauth_result = mgr
                        .run_oauth_flow_with_id(&flow_id, server_name, url, &cfg)
                        .await;
                    self.release_oauth_flow(server_name, &flow_id);
                    match oauth_result {
                        Ok(()) => {
                            used_oauth = true;
                            if let Some(am) = mgr.get_authorization_manager(server_name) {
                                serve_client_auto(
                                    build_authed_transport(url, headers, am),
                                    None,
                                    protocol_version,
                                    &self.capability_profile,
                                    timeout,
                                )
                                .await
                            } else {
                                serve_client_auto(
                                    build_http_transport(url, headers),
                                    None,
                                    protocol_version,
                                    &self.capability_profile,
                                    timeout,
                                )
                                .await
                            }
                        }
                        Err(e) => {
                            tracing::warn!(server = %server_name, error = %e, "OAuth 恢复失败，尝试裸连接");
                            serve_client_auto(
                                build_http_transport(url, headers),
                                None,
                                protocol_version,
                                &self.capability_profile,
                                timeout,
                            )
                            .await
                        }
                    }
                } else {
                    serve_client_auto(
                        build_http_transport(url, headers),
                        None,
                        protocol_version,
                        &self.capability_profile,
                        timeout,
                    )
                    .await
                }
            }
        };

        match result {
            Ok(Ok(rs)) => {
                let rs = self.retain_service(rs);
                // 订阅配置存在：按 server 配置重建 subscriptions/listen 长流
                // （2026-07-28）。失败仅告警——server 可能不支持，连接本身仍可用。
                if let Some(sub) = subscriptions {
                    setup_subscription(self, &rs, server_name, sub).await;
                }
                let peer = rs.peer().clone();
                let cache_version = self.install_peer_cache_version(server_name, &peer);
                // 严格发现：`tools/list` 的 `Err` 不是「没有工具」。System MCP 走
                // 本次 live round-trip（不用历史缓存代替健康证据），失败即
                // ToolDiscoveryFailed，不提交 Connected。
                let tools =
                    match list_discovered_tools(self, server_name, &peer, &server_config).await {
                        Ok(tools) => tools,
                        Err(error) => {
                            // 不留下「无句柄」的模糊状态：显式 Failed + 本代发现失败
                            // 证据，闸门据此立即判定，而不是等到 deadline。
                            fail_tool_discovery(self, server_name, &error.to_string());
                            return Err(McpPoolError::ToolDiscoveryFailed {
                                server: server_name.to_string(),
                                reason: error.to_string(),
                            });
                        }
                    };
                let resources = match self.list_all_resources_cached(server_name, &peer).await {
                    Ok(resources) => resources,
                    Err(error) => {
                        downgrade_resource_listing(server_name, &error.to_string());
                        Vec::new()
                    }
                };
                let skills_capable = super::client::peer_declares_skills(&peer);
                let oauth_status = if used_oauth {
                    OAuthStatus::Authorized
                } else {
                    OAuthStatus::default()
                };
                let handle = Arc::new(McpClientHandle {
                    name: server_name.to_string(),
                    version: peer
                        .peer_info()
                        .and_then(|info| info.server_info.as_ref().map(|si| si.version.clone())),
                    cache_version: cache_version.clone(),
                    peer: Some(peer),
                    tools,
                    resources,
                    status: ClientStatus::Connected,
                    oauth_status,
                    source: server_config.source.clone(),
                    url: server_config.url.clone(),
                    channel_capable: false,
                    skills_capable,
                });
                let committed = Arc::clone(&handle);
                if let Err(mut service) =
                    self.try_commit_connection(server_name.to_string(), handle, rs)
                {
                    let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
                    // 提交被拒（pool 关闭）：不留任何可被读成成功的证据。
                    self.clear_discovery_evidence(server_name);
                    return Err(McpPoolError::ConnectionFailed {
                        server: server_name.to_string(),
                        reason: "MCP pool is closing".to_string(),
                    });
                }
                self.record_status_change(server_name, old_status.as_ref());
                // 重连同样只能由真实成功的 live `tools/list` 产生本代发现证据。
                commit_discovery_success(self, server_name, &committed);
                Ok(())
            }
            Ok(Err(e)) => {
                let err_str = e.to_string();
                if McpClientPool::is_auth_required_error(&err_str, is_http) {
                    McpClientPool::insert_needs_auth(self, server_name, err_str.clone());
                } else {
                    McpClientPool::insert_failed(self, server_name, err_str.clone());
                }
                commit_discovery_failure(self, server_name, false);
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: err_str,
                })
            }
            Err(_) => {
                let msg = "连接超时";
                McpClientPool::insert_failed(self, server_name, msg.to_string());
                commit_discovery_failure(self, server_name, false);
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: msg.to_string(),
                })
            }
        }
    }
}
