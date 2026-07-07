use std::{path::Path, sync::Arc};

use super::{
    auth_store::FileCredentialStore,
    channel_handler::ChannelHandler,
    client::{
        build_authed_transport, build_http_transport, spawn_stdio_transport, ClientStatus,
        McpClientHandle, McpClientPool, McpInitStatus, McpServiceWrapper, OAuthStatus,
        HTTP_CONNECT_TIMEOUT, STDIO_CONNECT_TIMEOUT,
    },
    config::OAuthConfig,
    oauth_flow::{OAuthFlowEvent, OAuthFlowManager},
    transport::TransportConfig,
};

impl McpClientPool {
    pub async fn run_initialize(
        pool: Arc<Self>,
        cwd: &Path,
        claude_home: &Path,
        status_tx: tokio::sync::watch::Sender<McpInitStatus>,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) {
        let (config, plugin_sources) = super::load_merged_config_full(cwd, claude_home);
        let connectable = config
            .mcp_servers
            .iter()
            .filter(|(_, sc)| !sc.disabled.unwrap_or(false))
            .count();
        if config.mcp_servers.is_empty() {
            let _ = status_tx.send(McpInitStatus::Ready { total: 0 });
            return;
        }

        *pool.plugin_sources.write() = plugin_sources;

        let token_store = Arc::new(FileCredentialStore::new());
        let mut oauth_manager: Option<OAuthFlowManager> =
            oauth_event_callback.map(|cb| OAuthFlowManager::new(token_store, cb));

        for (name, server_config) in &config.mcp_servers {
            pool.configs
                .write()
                .insert(name.clone(), server_config.clone());
        }
        let _ = status_tx.send(McpInitStatus::Initializing {
            connected: 0,
            total: connectable,
        });

        let mut connected = 0usize;
        for (name, server_config) in &config.mcp_servers {
            // 跳过已禁用的服务器，注册为 Disabled 状态
            if server_config.disabled.unwrap_or(false) {
                tracing::info!(server = %name, "MCP 服务器已禁用，跳过连接");
                pool.clients.write().insert(
                    name.clone(),
                    Arc::new(McpClientHandle {
                        name: name.clone(),
                        peer: None,
                        tools: vec![],
                        resources: vec![],
                        status: ClientStatus::Disabled,
                        oauth_status: OAuthStatus::default(),
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable: false,
                    }),
                );
                continue;
            }
            let transport_config = match TransportConfig::try_from(server_config) {
                Ok(tc) => tc,
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "传输层构建失败");
                    Self::insert_failed(&pool, name, format!("传输层构建失败: {e}"));
                    continue;
                }
            };
            let is_http = matches!(transport_config, TransportConfig::StreamableHttp { .. });
            let timeout = if is_http {
                HTTP_CONNECT_TIMEOUT
            } else {
                STDIO_CONNECT_TIMEOUT
            };

            let mut used_oauth = false;
            let connect_result = match transport_config {
                TransportConfig::Stdio {
                    ref command,
                    ref args,
                    ref env,
                } => match spawn_stdio_transport(command, args, env) {
                    Ok(transport) => {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(handler.clone(), transport),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client((), transport),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                    Err(e) => {
                        Self::insert_failed(&pool, name, format!("stdio 启动失败: {e}"));
                        continue;
                    }
                },
                TransportConfig::StreamableHttp {
                    ref url,
                    ref headers,
                    ref oauth,
                } => {
                    let oauth_cfg = oauth.as_ref().cloned().or_else(|| {
                        if let Some(ref mgr) = oauth_manager {
                            let token_store = mgr.token_store();
                            match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(token_store.load_server(name))) {
                                Ok(Some(_)) => {
                                    tracing::info!(server = %name, "发现已保存的 OAuth 凭证，使用默认配置恢复");
                                    Some(OAuthConfig::default())
                                }
                                _ => None,
                            }
                        } else {
                            None
                        }
                    });
                    if let (Some(ref cfg), Some(ref mut mgr)) = (oauth_cfg, &mut oauth_manager) {
                        match mgr.run_oauth_flow(name, url, cfg).await {
                            Ok(()) => {
                                used_oauth = true;
                                if let Some(ref handler) = channel_handler {
                                    if let Some(am) = mgr.get_authorization_manager(name) {
                                        tokio::time::timeout(
                                            timeout,
                                            rmcp::service::serve_client(
                                                handler.clone(),
                                                build_authed_transport(url, headers, am),
                                            ),
                                        )
                                        .await
                                        .map(|inner| inner.map(McpServiceWrapper::Channel))
                                    } else {
                                        tokio::time::timeout(
                                            timeout,
                                            rmcp::service::serve_client(
                                                handler.clone(),
                                                build_http_transport(url, headers),
                                            ),
                                        )
                                        .await
                                        .map(|inner| inner.map(McpServiceWrapper::Channel))
                                    }
                                } else if let Some(am) = mgr.get_authorization_manager(name) {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            (),
                                            build_authed_transport(url, headers, am),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Default))
                                } else {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            (),
                                            build_http_transport(url, headers),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Default))
                                }
                            }
                            Err(e) => {
                                tracing::warn!(server = %name, error = %e, "OAuth 恢复失败，尝试裸连接");
                                if let Some(ref handler) = channel_handler {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            handler.clone(),
                                            build_http_transport(url, headers),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Channel))
                                } else {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            (),
                                            build_http_transport(url, headers),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Default))
                                }
                            }
                        }
                    } else {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(
                                    handler.clone(),
                                    build_http_transport(url, headers),
                                ),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client((), build_http_transport(url, headers)),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                }
            };

            match connect_result {
                Ok(Ok(rs)) => {
                    let tools = rs.list_all_tools().await.unwrap_or_default();
                    let resources = rs.list_all_resources().await.unwrap_or_default();
                    tracing::info!(server = %name, tools = tools.len(), resources = resources.len(), "MCP 连接成功");
                    let peer = rs.peer().clone();
                    let channel_capable = peer
                        .peer_info()
                        .and_then(|info| {
                            info.capabilities
                                .experimental
                                .as_ref()
                                .and_then(|exp| exp.get("claude/channel"))
                                .cloned()
                        })
                        .is_some();
                    let oauth_status = if used_oauth {
                        OAuthStatus::Authorized
                    } else {
                        OAuthStatus::default()
                    };
                    let handle = Arc::new(McpClientHandle {
                        name: name.clone(),
                        peer: Some(peer),
                        tools,
                        resources,
                        status: ClientStatus::Connected,
                        oauth_status,
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable,
                    });
                    pool.clients.write().insert(name.clone(), handle);
                    pool.services.lock().await.insert(name.clone(), rs);
                    connected += 1;
                    let _ = status_tx.send(McpInitStatus::Initializing {
                        connected,
                        total: connectable,
                    });
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    tracing::warn!(server = %name, error = %err_str, "MCP 连接失败");
                    if Self::is_auth_required_error(&err_str, is_http) {
                        Self::insert_needs_auth(&pool, name, err_str);
                    } else {
                        Self::insert_failed(&pool, name, err_str);
                    }
                }
                Err(_) => {
                    Self::insert_failed(&pool, name, "连接超时".to_string());
                }
            }
        }

        if connectable > 0 && connected == 0 {
            let all_need_auth = pool
                .clients
                .read()
                .values()
                .all(|h| h.oauth_status == OAuthStatus::NeedsAuthorization);
            if all_need_auth {
                let _ = status_tx.send(McpInitStatus::Ready { total: 0 });
            } else {
                let failed: Vec<String> = pool
                    .clients
                    .read()
                    .iter()
                    .filter(|(_, h)| matches!(h.status, ClientStatus::Failed(_)))
                    .map(|(n, h)| {
                        if let ClientStatus::Failed(r) = &h.status {
                            format!("{}: {}", n, r)
                        } else {
                            n.clone()
                        }
                    })
                    .collect();
                let _ = status_tx.send(McpInitStatus::Failed(format!(
                    "{} 个服务器连接失败: {}",
                    connectable,
                    failed.join("; ")
                )));
            }
        } else {
            let _ = status_tx.send(McpInitStatus::Ready { total: connected });
        }
    }

    pub async fn initialize(
        cwd: &Path,
        claude_home: &Path,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) -> Self {
        use std::collections::HashMap;

        let (config, plugin_sources) = super::load_merged_config_full(cwd, claude_home);
        let pool = Arc::new(Self::new_pending());
        *pool.plugin_sources.write() = plugin_sources;
        let token_store = Arc::new(FileCredentialStore::new());
        let mut oauth_manager: Option<OAuthFlowManager> =
            oauth_event_callback.map(|cb| OAuthFlowManager::new(token_store, cb));

        for (name, sc) in &config.mcp_servers {
            pool.configs.write().insert(name.clone(), sc.clone());
        }

        for (name, server_config) in &config.mcp_servers {
            // 跳过已禁用的服务器，注册为 Disabled 状态
            if server_config.disabled.unwrap_or(false) {
                tracing::info!(server = %name, "MCP 服务器已禁用，跳过连接");
                pool.clients.write().insert(
                    name.clone(),
                    Arc::new(McpClientHandle {
                        name: name.clone(),
                        peer: None,
                        tools: vec![],
                        resources: vec![],
                        status: ClientStatus::Disabled,
                        oauth_status: OAuthStatus::default(),
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable: false,
                    }),
                );
                continue;
            }
            let tc = match TransportConfig::try_from(server_config) {
                Ok(tc) => tc,
                Err(e) => {
                    Self::insert_failed(&pool, name, format!("传输层构建失败: {e}"));
                    continue;
                }
            };
            let is_http = matches!(tc, TransportConfig::StreamableHttp { .. });
            let timeout = if is_http {
                HTTP_CONNECT_TIMEOUT
            } else {
                STDIO_CONNECT_TIMEOUT
            };

            let mut used_oauth = false;
            let connect_result = match tc {
                TransportConfig::Stdio {
                    ref command,
                    ref args,
                    ref env,
                } => match spawn_stdio_transport(command, args, env) {
                    Ok(t) => {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(handler.clone(), t),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(timeout, rmcp::service::serve_client((), t))
                                .await
                                .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                    Err(e) => {
                        Self::insert_failed(&pool, name, format!("stdio 失败: {e}"));
                        continue;
                    }
                },
                TransportConfig::StreamableHttp {
                    ref url,
                    ref headers,
                    ref oauth,
                } => {
                    let oauth_cfg = oauth.as_ref().cloned().or_else(|| {
                        if let Some(ref mgr) = oauth_manager {
                            let token_store = mgr.token_store();
                            match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(token_store.load_server(name))) {
                                Ok(Some(_)) => {
                                    tracing::info!(server = %name, "发现已保存的 OAuth 凭证，使用默认配置恢复");
                                    Some(OAuthConfig::default())
                                }
                                _ => None,
                            }
                        } else {
                            None
                        }
                    });
                    if let (Some(ref cfg), Some(ref mut mgr)) = (oauth_cfg, &mut oauth_manager) {
                        match mgr.run_oauth_flow(name, url, cfg).await {
                            Ok(()) => {
                                used_oauth = true;
                                if let Some(ref handler) = channel_handler {
                                    if let Some(am) = mgr.get_authorization_manager(name) {
                                        tokio::time::timeout(
                                            timeout,
                                            rmcp::service::serve_client(
                                                handler.clone(),
                                                build_authed_transport(url, headers, am),
                                            ),
                                        )
                                        .await
                                        .map(|inner| inner.map(McpServiceWrapper::Channel))
                                    } else {
                                        tokio::time::timeout(
                                            timeout,
                                            rmcp::service::serve_client(
                                                handler.clone(),
                                                build_http_transport(url, headers),
                                            ),
                                        )
                                        .await
                                        .map(|inner| inner.map(McpServiceWrapper::Channel))
                                    }
                                } else if let Some(am) = mgr.get_authorization_manager(name) {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            (),
                                            build_authed_transport(url, headers, am),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Default))
                                } else {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            (),
                                            build_http_transport(url, headers),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Default))
                                }
                            }
                            Err(e) => {
                                tracing::warn!(server = %name, error = %e, "OAuth 恢复失败，尝试裸连接");
                                if let Some(ref handler) = channel_handler {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            handler.clone(),
                                            build_http_transport(url, headers),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Channel))
                                } else {
                                    tokio::time::timeout(
                                        timeout,
                                        rmcp::service::serve_client(
                                            (),
                                            build_http_transport(url, headers),
                                        ),
                                    )
                                    .await
                                    .map(|inner| inner.map(McpServiceWrapper::Default))
                                }
                            }
                        }
                    } else {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(
                                    handler.clone(),
                                    build_http_transport(url, headers),
                                ),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client((), build_http_transport(url, headers)),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                }
            };

            match connect_result {
                Ok(Ok(rs)) => {
                    let tools = rs.list_all_tools().await.unwrap_or_default();
                    let resources = rs.list_all_resources().await.unwrap_or_default();
                    let peer = rs.peer().clone();
                    let channel_capable = peer
                        .peer_info()
                        .and_then(|info| {
                            info.capabilities
                                .experimental
                                .as_ref()
                                .and_then(|exp| exp.get("claude/channel"))
                                .cloned()
                        })
                        .is_some();
                    let oauth_status = if used_oauth {
                        OAuthStatus::Authorized
                    } else {
                        OAuthStatus::default()
                    };
                    pool.clients.write().insert(
                        name.clone(),
                        Arc::new(McpClientHandle {
                            name: name.clone(),
                            peer: Some(peer),
                            tools,
                            resources,
                            status: ClientStatus::Connected,
                            oauth_status,
                            source: server_config.source.clone(),
                            url: server_config.url.clone(),
                            channel_capable,
                        }),
                    );
                    pool.services.lock().await.insert(name.clone(), rs);
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    if Self::is_auth_required_error(&err_str, is_http) {
                        Self::insert_needs_auth(&pool, name, err_str);
                    } else {
                        Self::insert_failed(&pool, name, err_str);
                    }
                }
                Err(_) => {
                    Self::insert_failed(&pool, name, "连接超时".into());
                }
            }
        }

        Arc::try_unwrap(pool).unwrap_or_else(|arc| {
            let p = arc.as_ref();
            Self {
                clients: parking_lot::RwLock::new(p.clients.read().clone()),
                services: tokio::sync::Mutex::new(HashMap::new()),
                configs: parking_lot::RwLock::new(p.configs.read().clone()),
                plugin_sources: parking_lot::RwLock::new(p.plugin_sources.read().clone()),
            }
        })
    }
}
