use std::{path::Path, sync::Arc};

use super::{
    auth_store::FileCredentialStore,
    channel_handler::ChannelHandler,
    client::{
        build_http_transport, serve_client_auto, setup_subscription, spawn_stdio_transport,
        ClientStatus, McpClientHandle, McpClientPool, McpInitStatus, OAuthStatus,
        HTTP_CONNECT_TIMEOUT, STDIO_CONNECT_TIMEOUT,
    },
    config::OAuthConfig,
    oauth_flow::OAuthFlowEvent,
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
            *pool.init_status.write() = McpInitStatus::Ready { total: 0 };
            pool.mark_initialized();
            return;
        }

        *pool.plugin_sources.write() = plugin_sources;

        // OAuth 事件回调注入 pool（spawn_oauth_flow / start_oauth_flow 读取；
        // 无回调时授权不自动触发——由 host pool 统一执行，本 pool 仅标记
        // NeedsAuthorization，授权完成后经共享凭证文件恢复）。
        if let Some(cb) = oauth_event_callback {
            pool.set_oauth_event_callback(cb);
        }
        let token_store = Arc::new(FileCredentialStore::new());

        for (name, server_config) in &config.mcp_servers {
            pool.configs
                .write()
                .insert(name.clone(), server_config.clone());
        }
        let _ = status_tx.send(McpInitStatus::Initializing {
            connected: 0,
            total: connectable,
        });
        *pool.init_status.write() = McpInitStatus::Initializing {
            connected: 0,
            total: connectable,
        };

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
            // subscriptions 配置存在时协商 2026-07-28 协议（Auto：先 server/discover，
            // 服务器不支持时回退 legacy 握手），否则维持原协商路径。
            let subscriptions = server_config
                .subscriptions
                .as_ref()
                .filter(|s| !s.is_empty());

            let connect_result = match transport_config {
                TransportConfig::Stdio {
                    ref command,
                    ref args,
                    ref env,
                } => match spawn_stdio_transport(command, args, env) {
                    Ok(transport) => {
                        serve_client_auto(
                            transport,
                            channel_handler.as_ref(),
                            subscriptions,
                            timeout,
                        )
                        .await
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
                        // 无显式 OAuth 配置时：若凭证文件已有该 server 的 token，
                        // 用默认配置走恢复路径（run_oauth_flow 快速路径跳过浏览器）。
                        match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(token_store.load_server(name))) {
                            Ok(Some(_)) => {
                                tracing::info!(server = %name, "发现已保存的 OAuth 凭证，使用默认配置恢复");
                                Some(OAuthConfig::default())
                            }
                            _ => None,
                        }
                    });
                    if oauth_cfg.is_some() {
                        if pool.oauth_event_callback().is_some() {
                            // host pool：不主动触发授权（避免启动即弹 popup
                            // 打扰），统一标记 NeedsAuthorization，由用户经
                            // MCP 面板显式发起（mcp/oauth_start RPC →
                            // spawn_oauth_flow → popup）。
                            Self::insert_needs_auth(&pool, name, "OAuth 授权待完成".to_string());
                            continue;
                        }
                        // TUI 面板池：无 UI 交互通道，走快速路径——尝试恢复
                        // 磁盘凭证直接连接（不弹窗）；凭据缺失/失效时保持
                        // NeedsAuthorization，由 host pool 授权后共享凭证文件
                        // 恢复。异步执行不阻塞初始化。
                        pool.spawn_oauth_flow(name);
                        continue;
                    } else {
                        serve_client_auto(
                            build_http_transport(url, headers),
                            channel_handler.as_ref(),
                            subscriptions,
                            timeout,
                        )
                        .await
                    }
                }
            };

            match connect_result {
                Ok(Ok(rs)) => {
                    // 订阅配置存在：建立 subscriptions/listen 长流（2026-07-28）。
                    // 失败仅告警——server 可能不支持，连接本身仍可用。
                    if let Some(sub) = subscriptions {
                        setup_subscription(&pool, &rs, name, sub).await;
                    }
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
                    let oauth_status = OAuthStatus::default();
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
                    *pool.init_status.write() = McpInitStatus::Initializing {
                        connected,
                        total: connectable,
                    };
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    tracing::warn!(server = %name, error = %err_str, "MCP 连接失败");
                    if Self::is_auth_required_error(&err_str, is_http) {
                        // 服务器要求授权（如 sentry 401）：标记待授权，不主动
                        // 触发——用户经 MCP 面板显式发起授权（mcp/oauth_start）。
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
                *pool.init_status.write() = McpInitStatus::Ready { total: 0 };
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
                *pool.init_status.write() = McpInitStatus::Failed(format!(
                    "{} 个服务器连接失败: {}",
                    connectable,
                    failed.join("; ")
                ));
            }
        } else {
            let _ = status_tx.send(McpInitStatus::Ready { total: connected });
            *pool.init_status.write() = McpInitStatus::Ready { total: connected };
        }
        // 初始化收口：此后状态变化才产生上下线通知（初始连接结果由
        // 会话首 turn 的 first_turn_reminder 概览覆盖，不逐条推送）。
        pool.mark_initialized();
    }

    pub async fn initialize(
        cwd: &Path,
        claude_home: &Path,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) -> Self {
        let (config, plugin_sources) = super::load_merged_config_full(cwd, claude_home);
        let pool = Arc::new(Self::new_pending());
        *pool.plugin_sources.write() = plugin_sources;
        let token_store = Arc::new(FileCredentialStore::new());
        // OAuth 事件回调注入 pool（spawn_oauth_flow / start_oauth_flow 读取；
        // 无回调时授权不自动触发，仅标记 NeedsAuthorization）。
        if let Some(cb) = oauth_event_callback {
            pool.set_oauth_event_callback(cb);
        }

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
            // subscriptions 配置存在时协商 2026-07-28 协议（Auto 协商），否则维持原路径。
            let subscriptions = server_config
                .subscriptions
                .as_ref()
                .filter(|s| !s.is_empty());

            let connect_result = match tc {
                TransportConfig::Stdio {
                    ref command,
                    ref args,
                    ref env,
                } => match spawn_stdio_transport(command, args, env) {
                    Ok(t) => {
                        serve_client_auto(t, channel_handler.as_ref(), subscriptions, timeout).await
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
                        // 无显式 OAuth 配置时：若凭证文件已有该 server 的 token，
                        // 用默认配置走恢复路径（run_oauth_flow 快速路径跳过浏览器）。
                        match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(token_store.load_server(name))) {
                            Ok(Some(_)) => {
                                tracing::info!(server = %name, "发现已保存的 OAuth 凭证，使用默认配置恢复");
                                Some(OAuthConfig::default())
                            }
                            _ => None,
                        }
                    });
                    if oauth_cfg.is_some() {
                        if pool.oauth_event_callback().is_some() {
                            // host pool：不主动触发授权（避免启动即弹 popup
                            // 打扰），统一标记 NeedsAuthorization，由用户经
                            // MCP 面板显式发起（mcp/oauth_start RPC →
                            // spawn_oauth_flow → popup）。
                            Self::insert_needs_auth(&pool, name, "OAuth 授权待完成".to_string());
                            continue;
                        }
                        // TUI 面板池：无 UI 交互通道，走快速路径——尝试恢复
                        // 磁盘凭证直接连接（不弹窗）；凭据缺失/失效时保持
                        // NeedsAuthorization，由 host pool 授权后共享凭证文件
                        // 恢复。异步执行不阻塞初始化。
                        pool.spawn_oauth_flow(name);
                        continue;
                    } else {
                        serve_client_auto(
                            build_http_transport(url, headers),
                            channel_handler.as_ref(),
                            subscriptions,
                            timeout,
                        )
                        .await
                    }
                }
            };

            match connect_result {
                Ok(Ok(rs)) => {
                    // 订阅配置存在：建立 subscriptions/listen 长流（2026-07-28）。
                    if let Some(sub) = subscriptions {
                        setup_subscription(&pool, &rs, name, sub).await;
                    }
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
                    let oauth_status = OAuthStatus::default();
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
                        // 服务器要求授权（如 sentry 401）：标记待授权，不主动
                        // 触发——用户经 MCP 面板显式发起授权（mcp/oauth_start）。
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
            let cloned = Self::new_pending();
            *cloned.clients.write() = p.clients.read().clone();
            *cloned.configs.write() = p.configs.read().clone();
            *cloned.plugin_sources.write() = p.plugin_sources.read().clone();
            *cloned.init_status.write() = p.init_status.read().clone();
            cloned.initialized.store(
                p.initialized.load(std::sync::atomic::Ordering::SeqCst),
                std::sync::atomic::Ordering::SeqCst,
            );
            cloned
        })
    }
}
