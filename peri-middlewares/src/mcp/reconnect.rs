use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::auth_store;
use super::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT;
use super::client;
use super::client::ClientStatus;
use super::client::McpClientHandle;
use super::client::McpClientPool;
use super::client::McpPoolError;
use super::client::McpServiceWrapper;
use super::client::OAuthStartDisposition;
use super::client::OAuthStatus;
use super::client::SHUTDOWN_TIMEOUT;
use super::config::McpServerConfig;
use super::initialize;
use super::oauth_flow::OAuthFlowEvent;
use super::oauth_flow::OAuthFlowManager;
use super::transport::TransportConfig;
use super::transport::TransportKind;

/// 一条已建立并通过发现验证、但尚未提交的连接。
pub(crate) struct ReadyConnection {
    pub(crate) handle: Arc<McpClientHandle>,
    pub(crate) service: McpServiceWrapper,
}

/// 共享连接逻辑的结果：已就绪，或 HTTP 需要交互式 OAuth。
pub(crate) enum ConnectOutcome {
    Connected(ReadyConnection),
    NeedsAuthorization,
}

impl McpClientPool {
    pub fn spawn_reconnect(
        self: &Arc<Self>,
        server_name: String,
    ) -> Result<(), super::task_scope::McpTaskScopeClosed> {
        let slot = self.server_slot(&server_name);
        if slot.stopped.load(Ordering::Acquire) || slot.control_operation.try_lock().is_err() {
            return Err(super::task_scope::TaskAdmissionError::OwnerClosed);
        }
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
        let slot = self.server_slot(server_name);
        let _control =
            slot.control_operation
                .try_lock()
                .map_err(|_| McpPoolError::NotConnected {
                    server: server_name.into(),
                    status: ClientStatus::Disconnected,
                })?;
        if slot.stopped.load(Ordering::Acquire) {
            return Err(McpPoolError::NotConnected {
                server: server_name.into(),
                status: ClientStatus::Disconnected,
            });
        }
        let _operation = slot.operation.clone().try_lock_owned().map_err(|_| {
            McpPoolError::ConnectionFailed {
                server: server_name.into(),
                reason: "MCP server is busy".into(),
            }
        })?;
        drop(_control);
        self.reconnect_managed(server_name, oauth_event_callback, true)
            .await
    }

    /// 重连：关闭既有连接后，复用共享连接逻辑重新建立并提交。
    pub(crate) async fn reconnect_managed(
        self: &Arc<Self>,
        server_name: &str,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        interactive_oauth: bool,
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
        self.close_server_connection(server_name, true)
            .await
            .map_err(|e| McpPoolError::ConnectionFailed {
                server: server_name.into(),
                reason: e.to_string(),
            })?;
        self.server_slot(server_name)
            .stopped
            .store(false, Ordering::Release);
        // 重连前捕获旧状态：insert 覆盖后由 record_status_change 判定是否
        // 构成上下线变化（Connected→Failed 等）；首次插入（旧状态不存在）
        // 不产生通知（初始化阶段由首 turn 概览覆盖）。
        let old_status = self
            .clients
            .read()
            .get(server_name)
            .map(|c| c.status.clone());
        self.clients.write().remove(server_name);
        match self
            .connect_managed(
                server_name,
                &server_config,
                oauth_event_callback,
                interactive_oauth,
                &|| true,
            )
            .await?
        {
            ConnectOutcome::NeedsAuthorization => Ok(()),
            ConnectOutcome::Connected(ready) => {
                self.commit_ready(server_name, ready, old_status, || true)
                    .await
            }
        }
    }

    /// 从受控配置建立并验证一条连接。
    ///
    /// 不关闭旧连接、不判断重连资格：transport / OAuth / 发现 / 句柄构造由首次加载、
    /// 初始化后的重连共用；关闭旧连接与提交时机由各自调用场景决定。
    /// `admit` 在发布失败证据前检查；迟到结果不得覆盖已经停止或换代的目标。
    pub(crate) async fn connect_managed(
        self: &Arc<Self>,
        server_name: &str,
        server_config: &McpServerConfig,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        interactive_oauth: bool,
        admit: &(dyn Fn() -> bool + Sync),
    ) -> Result<ConnectOutcome, McpPoolError> {
        if matches!(
            server_config.source,
            Some(super::config::ConfigSource::Builtin { .. })
        ) && !self.builtin_available.load(Ordering::Acquire)
        {
            let reason = "builtin handler is unavailable in this deployment".to_string();
            if admit() {
                McpClientPool::insert_failed(self, server_name, reason.clone());
                initialize::commit_discovery_failure(self, server_name, false);
            }
            return Err(McpPoolError::ConnectionFailed {
                server: server_name.to_string(),
                reason,
            });
        }

        let tc = TransportConfig::try_from(server_config).map_err(|e| {
            McpPoolError::ConnectionFailed {
                server: server_name.to_string(),
                reason: format!("传输层构建失败: {e}"),
            }
        })?;
        // 三分类（IF-D1）：超时与日志字段同源；`is_http` 仅用于 AuthRequired 判定。
        let kind = tc.kind();
        let timeout = initialize::connect_timeout(kind);
        let is_http = matches!(kind, TransportKind::Http);
        // lifecycle 自动协商；subscriptions 只负责连接后订阅。
        let subscriptions = server_config
            .subscriptions
            .as_ref()
            .filter(|s| !s.is_empty())
            .and_then(|subscriptions| self.filter_subscriptions(server_name, subscriptions));

        let mut used_oauth = false;
        let result = match &tc {
            // builtin 分支：重建一条**全新的**同进程链路（新 duplex + 新 handler 实例，
            // 旧 task 已在关闭路径收敛并移除）。与 stdio 路径同构：typed 原因 + 失败证据 +
            // `ConnectionFailed`，不 panic、不静默降级。
            TransportConfig::Builtin { instance } => {
                // 与 initialize 同一条构造入口：实例解析、上下文读取与 tick 挂载都在 pool
                // 方法内收口；本分支不再自行读 `execution_cwd`（cwd 由注入的上下文提供，
                // 与 pool 的 `execution_cwd` 同源）。
                let transport = match self.spawn_builtin_transport_with_environment(
                    instance,
                    server_config.env.as_ref().unwrap_or(&Default::default()),
                ) {
                    Ok(transport) => transport,
                    Err(error) => {
                        let reason = format!("builtin 启动失败: {error}");
                        if admit() {
                            McpClientPool::insert_failed(self, server_name, reason.clone());
                            initialize::commit_discovery_failure(self, server_name, false);
                        }
                        return Err(McpPoolError::ConnectionFailed {
                            server: server_name.to_string(),
                            reason,
                        });
                    }
                };
                let (io, supervisor) = transport.into_parts();
                if let Some(previous) =
                    self.register_builtin_task(server_name.to_string(), supervisor)
                {
                    let _ = previous.close(BUILTIN_CONVERGE_TIMEOUT).await;
                }
                let connected =
                    client::serve_client_auto_for_server(io, self, server_name, timeout).await;
                // 握手失败 / 超时：新链路当场收口，不留 orphan。
                if !matches!(connected, Ok(Ok(_))) {
                    self.close_builtin_task(server_name).await;
                }
                connected
            }
            #[cfg(not(target_os = "emscripten"))]
            TransportConfig::Stdio { command, args, env } => {
                if !self.stdio_available.load(Ordering::Acquire) {
                    let reason = crate::platform::STDIO_UNAVAILABLE.to_string();
                    if admit() {
                        McpClientPool::insert_failed(self, server_name, reason.clone());
                        initialize::commit_discovery_failure(self, server_name, false);
                    }
                    return Err(McpPoolError::ConnectionFailed {
                        server: server_name.to_string(),
                        reason,
                    });
                }
                let cwd =
                    self.execution_cwd
                        .get()
                        .ok_or_else(|| McpPoolError::ConnectionFailed {
                            server: server_name.to_owned(),
                            reason: "MCP execution directory is not initialized".into(),
                        })?;
                match self.spawn_stdio_transport(server_name, command, args, env, cwd) {
                    Ok(t) => {
                        client::serve_client_auto_for_server(t, self, server_name, timeout).await
                    }
                    Err(e) => {
                        let reason = format!("stdio 失败: {e}");
                        if admit() {
                            McpClientPool::insert_failed(self, server_name, reason.clone());
                            initialize::commit_discovery_failure(self, server_name, false);
                        }
                        return Err(McpPoolError::ConnectionFailed {
                            server: server_name.to_string(),
                            reason,
                        });
                    }
                }
            }
            #[cfg(target_os = "emscripten")]
            TransportConfig::Stdio { .. } => {
                return Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_owned(),
                    reason: crate::platform::STDIO_UNAVAILABLE.into(),
                });
            }
            TransportConfig::StreamableHttp {
                url,
                headers,
                oauth,
            } => {
                let token_store = self.oauth_credentials().ok();
                if oauth.is_some() && token_store.is_none() {
                    if !interactive_oauth {
                        if admit() {
                            McpClientPool::insert_needs_auth(
                                self,
                                server_name,
                                "OAuth authorization required".into(),
                            );
                        }
                        return Ok(ConnectOutcome::NeedsAuthorization);
                    }
                    let reason = "OAuth credentials were not injected".to_string();
                    if admit() {
                        McpClientPool::insert_failed(self, server_name, reason.clone());
                        initialize::commit_discovery_failure(self, server_name, false);
                    }
                    return Err(McpPoolError::ConnectionFailed {
                        server: server_name.to_string(),
                        reason,
                    });
                }
                let oauth_cfg = if let Some(config) = oauth.as_ref() {
                    Some(config.clone())
                } else if let Some(token_store) = token_store.as_ref() {
                    let default_oauth = super::config::OAuthConfig::default();
                    let key = auth_store::static_credential_key(server_name, url, &default_oauth);
                    token_store
                        .load_server(&key)
                        .await
                        .map_err(|error| McpPoolError::ConnectionFailed {
                            server: server_name.to_string(),
                            reason: error.to_string(),
                        })?
                        .map(|_| default_oauth)
                } else {
                    None
                };
                if let Some(cfg) = oauth_cfg {
                    let token_store = token_store.expect("OAuth path has credential client");
                    let flow_id = uuid::Uuid::now_v7().to_string();
                    // Control already holds exclusive admission. Quick restore never
                    // publishes a callback or reserves an interactive flow.
                    if interactive_oauth {
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
                    }
                    // callback 可空：优先调用方传入，其次 pool 级回调（host
                    // 装配注入，事件转发 TUI popup），都没有则 no-op 静默授权。
                    let cb: Arc<dyn Fn(OAuthFlowEvent) + Send + Sync> = oauth_event_callback
                        .map(Arc::from)
                        .or_else(|| self.oauth_event_callback())
                        .unwrap_or_else(|| Arc::new(|_| {}));
                    let mut mgr = OAuthFlowManager::new_with_arc(token_store, cb);
                    let oauth_result = if interactive_oauth {
                        mgr.run_oauth_flow_with_id(&flow_id, server_name, url, &cfg)
                            .await
                    } else {
                        mgr.restore_oauth_flow_with_id(&flow_id, server_name, url, &cfg)
                            .await
                    };
                    self.release_oauth_flow(server_name, &flow_id);
                    match oauth_result {
                        Ok(()) => {
                            used_oauth = true;
                            if let Some(am) = mgr.authorization_manager(server_name) {
                                client::serve_client_auto_for_server(
                                    client::build_authed_transport(url, headers, am),
                                    self,
                                    server_name,
                                    timeout,
                                )
                                .await
                            } else if oauth.is_none() {
                                client::serve_client_auto_for_server(
                                    client::build_http_transport(url, headers),
                                    self,
                                    server_name,
                                    timeout,
                                )
                                .await
                            } else {
                                let reason = "OAuth authorization produced no credential manager"
                                    .to_string();
                                if admit() {
                                    McpClientPool::insert_failed(self, server_name, reason.clone());
                                    initialize::commit_discovery_failure(self, server_name, false);
                                }
                                return Err(McpPoolError::ConnectionFailed {
                                    server: server_name.to_string(),
                                    reason,
                                });
                            }
                        }
                        Err(e) => {
                            if !interactive_oauth {
                                if admit() {
                                    McpClientPool::insert_needs_auth(
                                        self,
                                        server_name,
                                        "OAuth authorization required".into(),
                                    );
                                }
                                return Ok(ConnectOutcome::NeedsAuthorization);
                            }
                            if oauth.is_none() {
                                tracing::warn!(server = %server_name, error = %e, "OAuth 恢复失败，尝试裸连接");
                                client::serve_client_auto_for_server(
                                    client::build_http_transport(url, headers),
                                    self,
                                    server_name,
                                    timeout,
                                )
                                .await
                            } else {
                                let reason = format!("OAuth 恢复失败: {e}");
                                if admit() {
                                    McpClientPool::insert_failed(self, server_name, reason.clone());
                                    initialize::commit_discovery_failure(self, server_name, false);
                                }
                                return Err(McpPoolError::ConnectionFailed {
                                    server: server_name.to_string(),
                                    reason,
                                });
                            }
                        }
                    }
                } else {
                    client::serve_client_auto_for_server(
                        client::build_http_transport(url, headers),
                        self,
                        server_name,
                        timeout,
                    )
                    .await
                }
            }
        };

        match result {
            Ok(Ok(rs)) => {
                let rs = self.track_server_service(server_name, rs);
                // 订阅配置存在：按 server 配置重建 subscriptions/listen 长流
                // （2026-07-28）。失败仅告警——server 可能不支持，连接本身仍可用。
                // A24 关闭门与 initialize 同源（`pool.subscription_allowed` 唯一判定）。
                if let Some(sub) = &subscriptions {
                    if self.subscription_allowed(server_name) {
                        client::setup_subscription(self, &rs, server_name, sub).await;
                    } else {
                        tracing::info!(
                            server = %server_name,
                            "builtin 实例在关闭集内，跳过 subscriptions/listen"
                        );
                    }
                }
                let peer = rs.peer().clone();
                self.configure_peer_cache(&peer).await;
                let cache_version = self.install_peer_cache_version(server_name, &peer);
                // 严格发现：`tools/list` 的 `Err` 不是「没有工具」。System MCP 走
                // 本次 live round-trip（不用历史缓存代替健康证据），失败即
                // ToolDiscoveryFailed，不提交 Connected。
                let tools = match initialize::list_discovered_tools(
                    self,
                    server_name,
                    &peer,
                    server_config,
                )
                .await
                {
                    Ok(tools) => tools,
                    Err(error) => {
                        // 未提交连接必须当场收口：失败 attempt 不得留下半开 transport
                        // 或让后续重复加载累积 service/process。
                        let mut service = rs;
                        let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
                        self.release_stopped_resources(server_name);
                        // 不留下「无句柄」的模糊状态：显式 Failed + 本代发现失败
                        // 证据，闸门据此立即判定，而不是等到 deadline。
                        if admit() {
                            initialize::fail_tool_discovery(self, server_name, &error.to_string());
                        }
                        return Err(McpPoolError::ToolDiscoveryFailed {
                            server: server_name.to_string(),
                            reason: error.to_string(),
                        });
                    }
                };
                let resources = match self.list_all_resources_cached(server_name, &peer).await {
                    Ok(resources) => resources,
                    Err(error) => {
                        if matches!(
                            server_config.source,
                            Some(super::config::ConfigSource::WorkspaceRemote)
                        ) {
                            let mut service = rs;
                            let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
                            self.release_stopped_resources(server_name);
                            if admit() {
                                initialize::fail_workspace_resource_discovery(
                                    self,
                                    server_name,
                                    &error.to_string(),
                                );
                            }
                            return Err(McpPoolError::ResourceDiscoveryFailed {
                                server: server_name.to_string(),
                                reason: error.to_string(),
                            });
                        }
                        initialize::downgrade_resource_listing(server_name, &error.to_string());
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
                    call_gate: Default::default(),
                    name: server_name.to_string(),
                    version: peer
                        .peer_info()
                        .and_then(|info| info.server_info.as_ref().map(|si| si.version.clone())),
                    cache_version: cache_version.clone(),
                    connected_at: Some(peri_time::now_utc_rfc3339()),
                    protocol_version: peer
                        .peer_info()
                        .map(|info| info.protocol_version.as_str().to_string()),
                    peer: Some(peer),
                    tools,
                    resources,
                    status: ClientStatus::Connected,
                    oauth_status,
                    source: server_config.source.clone(),
                    url: server_config.url.clone(),
                    skills_capable,
                });
                Ok(ConnectOutcome::Connected(ReadyConnection {
                    handle,
                    service: rs,
                }))
            }
            Ok(Err(e)) => {
                let err_str = e.to_string();
                if admit() {
                    if McpClientPool::is_auth_required_error(&err_str, is_http) {
                        McpClientPool::insert_needs_auth(self, server_name, err_str.clone());
                    } else {
                        McpClientPool::insert_failed(self, server_name, err_str.clone());
                    }
                    initialize::commit_discovery_failure(self, server_name, false);
                }
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: err_str,
                })
            }
            Err(_) => {
                let msg = "连接超时";
                if admit() {
                    McpClientPool::insert_failed(self, server_name, msg.to_string());
                    initialize::commit_discovery_failure(self, server_name, false);
                }
                Err(McpPoolError::ConnectionFailed {
                    server: server_name.to_string(),
                    reason: msg.to_string(),
                })
            }
        }
    }

    /// 提交已验证连接：提交前再检查 pool / server / attempt 准入，并作为一组发布。
    pub(crate) async fn commit_ready(
        self: &Arc<Self>,
        server_name: &str,
        ready: ReadyConnection,
        old_status: Option<ClientStatus>,
        admit: impl FnOnce() -> bool,
    ) -> Result<(), McpPoolError> {
        let ReadyConnection { handle, service } = ready;
        if let Err(mut service) = self.try_commit_connection_guarded(
            server_name.to_string(),
            handle.clone(),
            service,
            admit,
        ) {
            // 提交被拒：关闭新获得的资源，不把旧结果挂回已停止的服务器。
            let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
            self.release_stopped_resources(server_name);
            self.clear_discovery_evidence(server_name);
            let reason = if self.is_open() {
                "MCP connection was superseded before commit"
            } else {
                "MCP pool is closing"
            };
            return Err(McpPoolError::ConnectionFailed {
                server: server_name.to_string(),
                reason: reason.to_string(),
            });
        }
        self.record_status_change(server_name, old_status.as_ref());
        // 重连同样只能由真实成功的 live `tools/list` 产生本代发现证据。
        initialize::commit_discovery_success(self, server_name, &handle);
        Ok(())
    }
}
