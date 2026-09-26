use std::{path::Path, sync::Arc};

use rmcp::{
    model::Tool,
    service::{Peer, RoleClient, ServiceError},
};

use super::{
    auth_store::FileCredentialStore,
    builtin::runtime::{spawn_builtin_transport, BuiltinTransport, BUILTIN_CONVERGE_TIMEOUT},
    channel_handler::ChannelHandler,
    client::{
        build_http_transport, serve_client_auto, setup_subscription, ClientStatus,
        DiscoveryEvidence, McpClientHandle, McpClientPool, McpInitStatus, OAuthStatus,
        SystemMcpManifest, HTTP_CONNECT_TIMEOUT, SHUTDOWN_TIMEOUT, STDIO_CONNECT_TIMEOUT,
    },
    config::{McpServerConfig, OAuthConfig},
    oauth_flow::OAuthFlowEvent,
    transport::{TransportConfig, TransportKind},
};

#[cfg(test)]
#[path = "initialize_test.rs"]
mod tests;

/// 三分类超时选择（IF-D1）：由传输形态决定，**禁止**再写成「http / 否则 stdio」的二元
/// 判定——那会让 builtin 复用 stdio 超时，并把失败日志的 `transport` 字段写成事实错误。
pub(super) fn connect_timeout(kind: TransportKind) -> std::time::Duration {
    match kind {
        TransportKind::Stdio => STDIO_CONNECT_TIMEOUT,
        TransportKind::Http => HTTP_CONNECT_TIMEOUT,
        TransportKind::Builtin => super::transport::BUILTIN_CONNECT_TIMEOUT,
    }
}

/// 与 [`connect_timeout`] 同源的日志字段（三分类三取值："stdio" | "http" | "builtin"）。
pub(super) fn transport_label(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::Stdio => "stdio",
        TransportKind::Http => "http",
        TransportKind::Builtin => "builtin",
    }
}

/// 启动发现的 `tools/list` 来源：System MCP 必须走本次 live round-trip。
///
/// System MCP 的 ready 证据不得来自历史持久缓存——缓存命中只说明「过去某次列出过
/// 这些工具」，不能证明本次启动的 server 仍能列出工具。`system_mcp_tools` 为空数组
/// 时同样要走完这次 round-trip（清单可以是空数组）。普通 MCP 保持既有 cache 策略。
///
/// `Err` 一律表示发现失败：既不等于「服务器没有工具」，也不产生任何 ready 证据。
pub(super) async fn list_discovered_tools(
    pool: &McpClientPool,
    server_name: &str,
    peer: &Peer<RoleClient>,
    config: &McpServerConfig,
) -> Result<Vec<Tool>, ServiceError> {
    if config.system_mcp == Some(true) {
        peer.list_all_tools().await
    } else {
        pool.list_all_tools_cached(server_name, peer).await
    }
}

/// 发现尝试的**成功**收口：证据绑定刚提交句柄的代际（`0` 表示未登记，读取方
/// 一律视为无效）。只有真实成功的 live `tools/list` 才走到这里。
pub(super) fn commit_discovery_success(
    pool: &McpClientPool,
    server_name: &str,
    committed: &Arc<McpClientHandle>,
) {
    let generation = pool.handle_generation(committed);
    pool.commit_discovery_evidence(server_name, DiscoveryEvidence::discovered(generation));
}

/// 发现尝试的**失败**收口。
///
/// `insert_failed` / `insert_needs_auth` 已经推进代际，证据必须绑定那个新句柄的
/// 代际，读取方才能把「本代已得出结论」与「仍在进行」区分开。没有句柄可绑定时
/// 清掉旧证据：宁可让等待方按「未完成」等到 deadline，也不留一条无法核对的成功。
pub(super) fn commit_discovery_failure(
    pool: &Arc<McpClientPool>,
    server_name: &str,
    initialize_ok: bool,
) {
    let Some(handle) = pool.get_client(server_name) else {
        pool.clear_discovery_evidence(server_name);
        return;
    };
    let generation = pool.handle_generation(&handle);
    let evidence = if initialize_ok {
        // initialize 成功但 live `tools/list` 失败：不是「空清单」，也不是完成。
        DiscoveryEvidence::discovery_failed(generation)
    } else {
        DiscoveryEvidence::initialize_failed(generation)
    };
    pool.commit_discovery_evidence(server_name, evidence);
}

/// `tools/list` 失败的统一收口：不提交 `Connected`（那会伪装成「发现完成且无工具」），
/// 改为显式 `Failed` + 本代 `tools_list_ok = false` 的证据。
pub(super) fn fail_tool_discovery(pool: &Arc<McpClientPool>, server_name: &str, error: &str) {
    let reason = format!("工具发现失败: {}", super::client::redact_mcp_error(error));
    tracing::warn!(server = %server_name, error = %reason, "MCP tools/list 失败，不发布连接与 ready 证据");
    McpClientPool::insert_failed(pool, server_name, reason);
    commit_discovery_failure(pool, server_name, true);
}

/// 资源清单的降级收口：resources 不是启动准入条件（契约 2 只冻结 transport /
/// initialize / 能力协商 / `tools/list`），连接与工具面保持可用；但解析失败不得
/// 静默写成「成功返回空列表」，必须留下可查的失败记录。
pub(super) fn downgrade_resource_listing(server_name: &str, error: &str) {
    tracing::warn!(
        server = %server_name,
        error = %super::client::redact_mcp_error(error),
        "MCP resources/list 失败，本次不发布资源"
    );
}

/// 配置失败的统一发布：面板状态（pool）与 watch 通道同时置 Failed，System 配置
/// 清单同时收口为 `Failed`。
///
/// 只发布失败，不 mark_initialized、不注册 server：失败不能退化成 ready 空配置，
/// 也不能让启动等待方把「配置加载失败」当「还没有 System MCP」睡到超时。
fn publish_config_failure(
    pool: &McpClientPool,
    status_tx: &tokio::sync::watch::Sender<McpInitStatus>,
    message: &str,
) {
    pool.publish_system_manifest(SystemMcpManifest::Failed);
    let status = McpInitStatus::Failed(message.to_string());
    *pool.init_status.write() = status.clone();
    let _ = status_tx.send(status);
}

impl McpClientPool {
    pub async fn run_initialize(
        pool: Arc<Self>,
        cwd: &Path,
        claude_home: &Path,
        status_tx: tokio::sync::watch::Sender<McpInitStatus>,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) {
        // 配置加载失败必须是可见的 Failed：不发布 Ready、不标记 initialized、
        // 不注册任何 server（因而也不会开始 transport）。B 在 1R 消费该失败。
        let (config, plugin_sources) = match super::load_merged_config_full(cwd, claude_home) {
            Ok(loaded) => loaded,
            Err(error) => {
                publish_config_failure(&pool, &status_tx, &error.to_string());
                return;
            }
        };
        Self::initialize_config(
            pool,
            cwd,
            config,
            plugin_sources,
            status_tx,
            oauth_event_callback,
            channel_handler,
        )
        .await;
    }

    async fn initialize_config(
        pool: Arc<Self>,
        cwd: &Path,
        config: super::config::McpConfigFile,
        plugin_sources: std::collections::HashMap<String, String>,
        status_tx: tokio::sync::watch::Sender<McpInitStatus>,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) {
        // typed 配置（含手工构造）在任何 empty / disabled / ready 分支之前校验：
        // 非法组合必须暴露为 Failed，不能因为「空配置」或「全部 disabled」被跳过。
        if let Err(error) = super::config::validate_config(&config) {
            publish_config_failure(&pool, &status_tx, &error.to_string());
            return;
        }
        let cwd = match pool.bind_execution_cwd(cwd) {
            Ok(cwd) => cwd,
            Err(error) => {
                // 执行目录绑定失败后本 pool 不可能再发布配置清单：等待方必须立刻
                // 拿到终态，而不是把「不可用」睡成 bootstrap 超时。
                pool.publish_system_manifest(SystemMcpManifest::Failed);
                let status = McpInitStatus::Failed(error.to_string());
                *pool.init_status.write() = status.clone();
                let _ = status_tx.send(status);
                return;
            }
        };
        let connectable = config
            .mcp_servers
            .iter()
            .filter(|(_, sc)| !sc.disabled.unwrap_or(false))
            .count();
        if config.mcp_servers.is_empty() {
            // 空集合也是**完整**清单：此后 system 依赖（零个）是可信事实。
            pool.publish_system_manifest(SystemMcpManifest::Loaded);
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
        // 完整 configs（含全部 disabled）已一次性写入：此刻起配置清单是可信的
        // System 依赖事实源，等待方可以按 server 逐个判定发现证据。
        pool.publish_system_manifest(SystemMcpManifest::Loaded);
        let _ = status_tx.send(McpInitStatus::Initializing {
            connected: 0,
            total: connectable,
        });
        *pool.init_status.write() = McpInitStatus::Initializing {
            connected: 0,
            total: connectable,
        };

        // 启动依赖优先推进：System MCP 是本轮启动的阻塞条件，普通 MCP 的慢
        // transport / 慢握手不得把它排在后面（原实现按 HashMap 顺序串行连接）。
        // 同优先级内按 server 名排序，使连接顺序确定、可复现。
        let mut ordered: Vec<(&String, &McpServerConfig)> = config.mcp_servers.iter().collect();
        ordered.sort_by(|(left_name, left), (right_name, right)| {
            let left_system = left.system_mcp == Some(true);
            let right_system = right.system_mcp == Some(true);
            right_system
                .cmp(&left_system)
                .then_with(|| left_name.cmp(right_name))
        });

        let mut connected = 0usize;
        for (name, server_config) in ordered {
            // 跳过已禁用的服务器，注册为 Disabled 状态
            if server_config.disabled.unwrap_or(false) {
                tracing::info!(server = %name, "MCP 服务器已禁用，跳过连接");
                pool.clients.write().insert(
                    name.clone(),
                    Arc::new(McpClientHandle {
                        name: name.clone(),
                        version: None,
                        cache_version: None,
                        peer: None,
                        tools: vec![],
                        resources: vec![],
                        status: ClientStatus::Disabled,
                        oauth_status: OAuthStatus::default(),
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        skills_capable: false,
                        channel_capable: false,
                    }),
                );
                continue;
            }
            // 本次发现尝试开始：旧代证据立即作废，等待方按「仍在进行」重新判定，
            // 不会把上一次尝试的成功当成这一次的证据。
            pool.clear_discovery_evidence(name);
            let transport_config = match TransportConfig::try_from(server_config) {
                Ok(tc) => tc,
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "传输层构建失败");
                    Self::insert_failed(&pool, name, format!("传输层构建失败: {e}"));
                    commit_discovery_failure(&pool, name, false);
                    continue;
                }
            };
            // 三分类（IF-D1）：超时与日志字段来自同一结果；`is_http` 仅用于
            // 「是否 AuthRequired」判定（HTTP 专属，builtin 无 URL / 无凭据恒 false）。
            let kind = transport_config.kind();
            let timeout = connect_timeout(kind);
            let is_http = matches!(kind, TransportKind::Http);
            // lifecycle 仅由显式 protocolVersion 选择；subscriptions 只负责连接后订阅。
            let protocol_version = server_config.protocol_version.as_ref();
            let subscriptions = server_config
                .subscriptions
                .as_ref()
                .filter(|s| !s.is_empty());

            let connect_result = match transport_config {
                // builtin 分支：同进程链路（duplex + 真实 `rmcp::serve_server`）。它与 stdio
                // 分支走**同一条**处理链（同参的 `serve_client_auto` → 发现 → 句柄 → 提交），
                // 不为 builtin 另起一套 spawn / readiness / 面板语义。
                TransportConfig::Builtin { ref instance } => {
                    // 实例解析在 spawn 内统一收口：未注册 → typed 原因 + 失败证据 + continue。
                    let transport = match spawn_builtin_transport(instance, cwd) {
                        Ok(transport) => transport,
                        Err(error) => {
                            let reason = format!("builtin 启动失败: {error}");
                            tracing::warn!(server = %name, error = %reason, "MCP builtin 启动失败");
                            Self::insert_failed(&pool, name, reason);
                            commit_discovery_failure(&pool, name, false);
                            continue;
                        }
                    };
                    let BuiltinTransport { io, server_task } = transport;
                    // task 归属：server 半边是同进程 task，必须与 `services` 同期登记，
                    // 否则重连 / 关闭会留下 orphan（IF-D12）。同名旧 task 先有界收敛。
                    if let Some(mut previous) =
                        pool.register_builtin_task(name.clone(), server_task)
                    {
                        let _ = previous.converge(BUILTIN_CONVERGE_TIMEOUT).await;
                    }
                    let connected = serve_client_auto(
                        io,
                        channel_handler.as_ref(),
                        protocol_version,
                        &pool.capability_profile,
                        timeout,
                    )
                    .await;
                    // 握手失败 / 超时：本实例的 server task 当场收口（不含糊到 pool 关闭）；
                    // 成功则由重连 / 关闭 / 移除时的有界关闭负责。
                    if !matches!(connected, Ok(Ok(_))) {
                        pool.close_builtin_task(name).await;
                    }
                    connected
                }
                TransportConfig::Stdio {
                    ref command,
                    ref args,
                    ref env,
                } => match pool.spawn_stdio_transport(command, args, env, cwd) {
                    Ok(transport) => {
                        serve_client_auto(
                            transport,
                            channel_handler.as_ref(),
                            protocol_version,
                            &pool.capability_profile,
                            timeout,
                        )
                        .await
                    }
                    Err(e) => {
                        let err_str = super::client::redact_mcp_error(&e.to_string());
                        tracing::warn!(server = %name, error = %err_str, "MCP stdio 启动失败");
                        Self::insert_failed(&pool, name, format!("stdio 启动失败: {err_str}"));
                        commit_discovery_failure(&pool, name, false);
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
                            protocol_version,
                            &pool.capability_profile,
                            timeout,
                        )
                        .await
                    }
                }
            };

            match connect_result {
                Ok(Ok(rs)) => {
                    let rs = pool.retain_service(rs);
                    // 订阅配置存在：建立 subscriptions/listen 长流（2026-07-28）。
                    // 失败仅告警——server 可能不支持，连接本身仍可用。
                    if let Some(sub) = subscriptions {
                        setup_subscription(&pool, &rs, name, sub).await;
                    }
                    let peer = rs.peer().clone();
                    let cache_version = pool.install_peer_cache_version(name, &peer);
                    // 严格发现（契约 2 / 主 plan IF-M3）：`tools/list` 的 `Err` 既不是
                    // 「服务器没有工具」，也不是 ready 证据。只有真实成功的 round-trip
                    // 才允许提交 `Connected`；失败必须显式失败并释放已建立的 service，
                    // 否则 `Connected + tools=[]` 会被下游误判为 discovery 完成。
                    let tools = match list_discovered_tools(&pool, name, &peer, server_config).await
                    {
                        Ok(tools) => tools,
                        Err(error) => {
                            let mut service = rs;
                            let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
                            fail_tool_discovery(&pool, name, &error.to_string());
                            continue;
                        }
                    };
                    let resources = match pool.list_all_resources_cached(name, &peer).await {
                        Ok(resources) => resources,
                        Err(error) => {
                            downgrade_resource_listing(name, &error.to_string());
                            Vec::new()
                        }
                    };
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
                    let skills_capable = super::client::peer_declares_skills(&peer);
                    let handle = Arc::new(McpClientHandle {
                        name: name.clone(),
                        version: peer.peer_info().and_then(|info| {
                            info.server_info.as_ref().map(|si| si.version.clone())
                        }),
                        cache_version: cache_version.clone(),
                        peer: Some(peer),
                        tools,
                        resources,
                        status: ClientStatus::Connected,
                        oauth_status,
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable,
                        skills_capable,
                    });
                    let committed = Arc::clone(&handle);
                    if let Err(mut service) = pool.try_commit_connection(name.clone(), handle, rs) {
                        let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
                        // 提交被拒（pool 关闭）：不留任何可被读成成功的证据。
                        pool.clear_discovery_evidence(name);
                        break;
                    }
                    // 唯一能提交「本代完整发现证据」的位置：transport + initialize +
                    // 能力协商 + 真实成功的 live `tools/list` 全部完成，且句柄已登记代际。
                    commit_discovery_success(&pool, name, &committed);
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
                    let err_str = super::client::redact_mcp_error(&e.to_string());
                    tracing::warn!(server = %name, error = %err_str, "MCP 连接失败");
                    if Self::is_auth_required_error(&err_str, is_http) {
                        // 服务器要求授权（如 sentry 401）：标记待授权，不主动
                        // 触发——用户经 MCP 面板显式发起授权（mcp/oauth_start）。
                        Self::insert_needs_auth(&pool, name, err_str);
                    } else {
                        Self::insert_failed(&pool, name, err_str);
                    }
                    // initialize 未完成：证据必须明确记为「本代 initialize 失败」，
                    // 不能靠「没有证据」让等待方一直等到 deadline。
                    commit_discovery_failure(&pool, name, false);
                }
                Err(_) => {
                    // 超时是面板上可见、日志里必须可查的失败：首次启动需装依赖的
                    // stdio 服务器会在连接超时内完不成握手。
                    tracing::warn!(
                        server = %name,
                        transport = transport_label(kind),
                        timeout_secs = timeout.as_secs(),
                        "MCP 连接超时"
                    );
                    Self::insert_failed(&pool, name, "连接超时".to_string());
                    commit_discovery_failure(&pool, name, false);
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
        // 初始连接补发（决策 B 扩展）：mark_initialized 之后为每个已连接
        // server 补发一次连接通知——`run_initialize` 直接插入 Connected
        // handle，初始化期间的连接事件不产生 record_status_change，挂载
        // 的连接事件 notifier（装配面 / session 预热）收不到初始连接。
        // 补发使「刚进入、未说话」场景下连接完成的 server 立即驱动
        // skill 发现（notifier 未挂载时零操作，由 session/new 预热发现
        // 兜底——get_all_clients 已非空）。
        pool.notify_initial_connections();
    }

    /// 测试用构造：直接复用生产初始化路径。
    ///
    /// 不保留第二套宽松的发现/提交逻辑——两条路径分叉时，测试会在与生产不同的
    /// 语义上变绿（`tools/list` 失败必须是显式 `Failed`，不是 `Connected` 空工具）。
    #[cfg(test)]
    pub async fn initialize(
        cwd: &Path,
        claude_home: &Path,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) -> Arc<Self> {
        let pool = Arc::new(Self::new_pending());
        let (config, plugin_sources) = match super::load_merged_config_full(cwd, claude_home) {
            Ok(loaded) => loaded,
            Err(error) => {
                *pool.init_status.write() = McpInitStatus::Failed(error.to_string());
                return pool;
            }
        };
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        Self::initialize_config(
            pool.clone(),
            cwd,
            config,
            plugin_sources,
            status_tx,
            oauth_event_callback,
            channel_handler,
        )
        .await;
        pool
    }
}
