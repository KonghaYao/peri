use super::{
    client, fmt, transport, AcpBridgeHandle, AcpMcpError, AcpMcpGatewayPort, AcpMcpInbound,
    AcpMcpRegistry, AcpMcpServerPort, AcpMcpServerSpec, Arc, BTreeMap, ClientStatus, ConfigSource,
    Debug, Formatter, HashMap, McpClientHandle, McpClientPool, McpTaskKey, Mutex, OAuthStatus,
    Value, HTTP_CONNECT_TIMEOUT, SHUTDOWN_TIMEOUT,
};

/// 会话级 MCP over ACP 服务（`AcpMcpServerPort` 实现）。
///
/// 会话级实例：构造点持具体类型，host 侧只持
/// `Arc<dyn AcpMcpServerPort>`。内部状态（会话 → 声明 → 连接）与 MCP 池
/// 分离，池只承担「已建立连接」的登记与工具面投影。
pub struct AcpMcpService {
    pub(super) pool: Arc<McpClientPool>,
    pub(super) state: Arc<Mutex<State>>,
}

impl Debug for AcpMcpService {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpMcpService")
            .finish_non_exhaustive()
    }
}

/// 服务内部状态：会话声明与活跃连接索引。
///
/// `sessions` 用 `BTreeMap` 固定遍历顺序（发现派发顺序可复现）；`servers`
/// 是 `serverId` → 桥接句柄的索引，入站 `mcp/message` 通知依赖它路由；
/// 连接级 `registry` 覆盖「正在发现」「已就绪」和「失败待重试」，保证
/// `serverId` 在会话释放声明前不可被其他会话占用。
#[derive(Default)]
pub(super) struct State {
    pub(super) sessions: BTreeMap<String, SessionRecord>,
    pub(super) servers: HashMap<String, ServerRecord>,
    pub(super) registry: Option<Arc<AcpMcpRegistry>>,
}

pub(super) struct SessionRecord {
    /// `serverId` → 发现状态（同一会话内 `serverId` 唯一）。
    pub(super) servers: BTreeMap<String, ServerPhase>,
}

/// 单个声明的发现状态。
///
/// 失败原因不在本状态机里留存：事实源是 MCP 池（`record_acp_failure`），
/// 本枚举只回答「是否需要再发现一次」。
pub(super) enum ServerPhase {
    Discovering,
    Ready,
    Failed,
}

pub(super) struct ServerRecord {
    pub(super) session_id: String,
    pub(super) handle: Arc<AcpBridgeHandle>,
}

impl AcpMcpService {
    #[must_use]
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            pool,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// 连接级注册先于发现；冲突明确返回 CODE_UNAVAILABLE，不静默跳过。
    pub(super) fn dispatch(
        &self,
        registry: &Arc<AcpMcpRegistry>,
        spec: AcpMcpServerSpec,
    ) -> Result<Option<AcpMcpServerSpec>, AcpMcpError> {
        let mut state = self.state.lock();
        if state
            .registry
            .as_ref()
            .is_some_and(|current| !Arc::ptr_eq(current, registry))
        {
            return Err(AcpMcpError::unavailable("MCP 服务已绑定另一条 ACP 连接"));
        }
        registry.claim(&spec.server_id, &spec.session_id)?;
        state.registry = Some(registry.clone());
        let record = state
            .sessions
            .entry(spec.session_id.clone())
            .or_insert_with(|| SessionRecord {
                servers: BTreeMap::new(),
            });
        match record.servers.get(&spec.server_id) {
            Some(ServerPhase::Discovering | ServerPhase::Ready) => Ok(None),
            // 失败过的声明允许重试（client 可在后续 session setup 中重发）。
            Some(ServerPhase::Failed) | None => {
                record
                    .servers
                    .insert(spec.server_id.clone(), ServerPhase::Discovering);
                Ok(Some(spec))
            }
        }
    }

    /// 发现任务的派发壳：任务本身失败不回抛，只记状态。
    pub(super) fn spawn_discover(
        pool: Arc<McpClientPool>,
        state: Arc<Mutex<State>>,
        gateway: Arc<dyn AcpMcpGatewayPort>,
        spec: AcpMcpServerSpec,
    ) {
        let key = McpTaskKey::Acp {
            session_id: spec.session_id.clone(),
            server_id: spec.server_id.clone(),
        };
        let session_id = spec.session_id.clone();
        let server_id = spec.server_id.clone();
        let name = preferred_name(&spec);
        let record_pool = Arc::clone(&pool);
        let task_pool = Arc::clone(&pool);
        let message = match pool.spawn_background(
            key,
            discover_server(task_pool, Arc::clone(&state), gateway, spec),
        ) {
            Ok(()) => return,
            // 池已关闭 / 同键任务仍在跑：连接不可能建立，如实记为失败。
            Err(error) => error.to_string(),
        };
        record_failure(
            &record_pool,
            &state,
            &session_id,
            &server_id,
            &name,
            &message,
        );
    }

    /// `serverId` → 桥接句柄。
    pub(super) fn route(&self, server_id: &str) -> Result<Arc<AcpBridgeHandle>, AcpMcpError> {
        self.state
            .lock()
            .servers
            .get(server_id)
            .map(|connection| Arc::clone(&connection.handle))
            .ok_or_else(|| {
                AcpMcpError::not_found(format!("未知或已关闭的 MCP-over-ACP server: {server_id}"))
            })
    }
}

#[async_trait::async_trait]
impl AcpMcpServerPort for AcpMcpService {
    fn attach(
        &self,
        registry: Arc<AcpMcpRegistry>,
        gateway: Arc<dyn AcpMcpGatewayPort>,
        servers: Vec<AcpMcpServerSpec>,
    ) -> Result<(), AcpMcpError> {
        for spec in servers {
            let Some(spec) = self.dispatch(&registry, spec)? else {
                continue;
            };
            Self::spawn_discover(
                Arc::clone(&self.pool),
                Arc::clone(&self.state),
                Arc::clone(&gateway),
                spec,
            );
        }
        Ok(())
    }

    async fn notify(&self, inbound: AcpMcpInbound) -> Result<(), AcpMcpError> {
        let handle = self.route(&inbound.server_id)?;
        handle
            .notify(
                &inbound.request_id,
                &inbound.method,
                inbound.params.map(Value::Object),
            )
            .map_err(|error| AcpMcpError::unavailable(error.message.to_string()))
    }

    fn owns_server(&self, server_id: &str) -> bool {
        self.state.lock().servers.contains_key(server_id)
    }

    async fn close_session(&self, session_id: &str) {
        // 顺序固定：先摘会话声明与路由索引（发现任务的存活检查据此判定），
        // 再终止在建任务，最后移除池条目——被 abort 的任务不会在此之后提交。
        let (server_ids, connections) = {
            let mut state = self.state.lock();
            let Some(record) = state.sessions.remove(session_id) else {
                return;
            };
            let connections: Vec<(String, Arc<AcpBridgeHandle>)> = state
                .servers
                .iter()
                .filter(|(_, connection)| connection.session_id == session_id)
                .map(|(server_id, connection)| (server_id.clone(), Arc::clone(&connection.handle)))
                .collect();
            for (server_id, _) in &connections {
                state.servers.remove(server_id);
            }
            (record.servers.into_keys().collect::<Vec<_>>(), connections)
        };

        for (_, handle) in &connections {
            handle.close().await;
        }
        for server_id in server_ids {
            self.pool
                .stop_background(&McpTaskKey::Acp {
                    session_id: session_id.to_string(),
                    server_id,
                })
                .await;
        }

        for (server_id, _) in connections {
            tracing::debug!(
                session_id = %session_id,
                server_id = %server_id,
                "MCP over ACP 桥接已关闭"
            );
        }

        if let Some(registry) = &self.state.lock().registry {
            registry.release_session(session_id);
        }
        let removed = self.pool.remove_acp_servers_for_session(session_id).await;
        if !removed.is_empty() {
            tracing::info!(
                session_id = %session_id,
                servers = removed.len(),
                "MCP over ACP 会话连接已清理"
            );
        }
    }
}

/// 发现任务主体：建桥接 → rmcp 协商 → 工具发现 → 池提交。
///
/// 失败路径只注销路由并记失败；协议没有 `mcp/disconnect`，对端按声明所在
/// 会话的存续回收。
pub(super) async fn discover_server(
    pool: Arc<McpClientPool>,
    state: Arc<Mutex<State>>,
    gateway: Arc<dyn AcpMcpGatewayPort>,
    spec: AcpMcpServerSpec,
) {
    let (transport, handle) = transport::create_bridge(gateway, spec.server_id.clone());
    // 协商期间 client 侧 server 就可能下发通知，入站路由必须早于协商注册。
    if !register_connection(&state, &spec, Arc::clone(&handle)) {
        handle.close().await;
        return;
    }

    let served =
        client::serve_client_auto(transport, &pool.capability_profile, HTTP_CONNECT_TIMEOUT).await;
    let service = match served {
        Ok(Ok(service)) => service,
        Ok(Err(error)) => {
            let message = error.to_string();
            fail_connection(&pool, &state, &spec, message).await;
            return;
        }
        Err(_) => {
            fail_connection(
                &pool,
                &state,
                &spec,
                format!("MCP 握手超时（{}s）", HTTP_CONNECT_TIMEOUT.as_secs()),
            )
            .await;
            return;
        }
    };

    let mut service = pool.retain_service(service);
    let peer = service.peer().clone();
    pool.configure_peer_cache(&peer).await;
    let tools = match peer.list_all_tools().await {
        Ok(tools) => tools,
        Err(error) => {
            let message = error.to_string();
            let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
            fail_connection(&pool, &state, &spec, message).await;
            return;
        }
    };
    if !connection_is_live(&state, &spec.server_id) {
        // 会话在协商 / 发现期间关闭：连接不落池，立即关闭桥接。
        let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        unregister_connection(&state, &spec.server_id).await;
        return;
    }

    let client = McpClientHandle {
        call_gate: Default::default(),
        name: preferred_name(&spec),
        version: peer.peer_info().and_then(|info| {
            info.server_info
                .as_ref()
                .map(|server| server.version.clone())
        }),
        // 会话级连接的来源是声明它的 client，不参与持久缓存。
        cache_version: None,
        connected_at: Some(peri_time::now_utc_rfc3339()),
        protocol_version: peer
            .peer_info()
            .map(|info| info.protocol_version.as_str().to_string()),
        skills_capable: client::peer_declares_skills(&peer),
        peer: Some(peer),
        resources: Vec::new(),
        tools,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: Some(ConfigSource::Acp),
        url: None,
    };
    match pool.commit_acp_connection(
        &spec.session_id,
        &spec.server_id,
        &spec.name,
        Arc::new(client),
        service,
    ) {
        Ok(pool_name) => {
            mark_ready(&state, &spec.session_id, &spec.server_id);
            tracing::info!(
                session_id = %spec.session_id,
                server = %spec.name,
                pool_name = %pool_name,
                "MCP over ACP 连接就绪"
            );
        }
        Err(mut service) => {
            // 池已关闭：不留任何可被读成成功的证据。
            let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
            unregister_connection(&state, &spec.server_id).await;
            mark_failed(&state, &spec.session_id, &spec.server_id);
        }
    }
}

/// 池内名：client 声明名为空时退回 `serverId`（工具命名要求非空前缀）。
pub(super) fn preferred_name(spec: &AcpMcpServerSpec) -> String {
    let name = spec.name.trim();
    if name.is_empty() {
        spec.server_id.clone()
    } else {
        name.to_string()
    }
}

/// 注册入站路由；返回 false 表示会话已关闭或 `serverId` 已被占用
/// （调用方必须立刻关闭桥接）。
pub(super) fn register_connection(
    state: &Mutex<State>,
    spec: &AcpMcpServerSpec,
    handle: Arc<AcpBridgeHandle>,
) -> bool {
    let mut state = state.lock();
    if !state.sessions.contains_key(&spec.session_id) {
        return false;
    }
    if state
        .registry
        .as_ref()
        .and_then(|registry| registry.owner(&spec.server_id))
        .as_deref()
        != Some(&spec.session_id)
    {
        return false;
    }
    state.servers.insert(
        spec.server_id.clone(),
        ServerRecord {
            session_id: spec.session_id.clone(),
            handle,
        },
    );
    true
}

pub(super) async fn unregister_connection(state: &Mutex<State>, server_id: &str) {
    let handle = state
        .lock()
        .servers
        .get(server_id)
        .map(|connection| connection.handle.clone());
    if let Some(handle) = handle {
        // 完成 join 前保留句柄，close_session 即使并发也能等待同一关闭事务。
        handle.close().await;
        state.lock().servers.remove(server_id);
    }
}

/// 连接是否仍属于存活会话（会话关闭后不得提交进池）。
pub(super) fn connection_is_live(state: &Mutex<State>, server_id: &str) -> bool {
    state.lock().servers.contains_key(server_id)
}

/// 失败收口：注销路由、池状态面记失败。
pub(super) async fn fail_connection(
    pool: &Arc<McpClientPool>,
    state: &Mutex<State>,
    spec: &AcpMcpServerSpec,
    message: String,
) {
    tracing::warn!(
        session_id = %spec.session_id,
        server = %spec.name,
        error = %message,
        "MCP over ACP 连接失败"
    );
    unregister_connection(state, &spec.server_id).await;
    record_failure(
        pool,
        state,
        &spec.session_id,
        &spec.server_id,
        &preferred_name(spec),
        &message,
    );
}

/// 失败收口（统一入口）：池状态面留失败条目 + 状态机置 `Failed`。
pub(super) fn record_failure(
    pool: &Arc<McpClientPool>,
    state: &Mutex<State>,
    session_id: &str,
    server_id: &str,
    name: &str,
    message: &str,
) {
    pool.record_acp_failure(session_id, name, message);
    mark_failed(state, session_id, server_id);
}

pub(super) fn mark_ready(state: &Mutex<State>, session_id: &str, server_id: &str) {
    if let Some(phase) = state
        .lock()
        .sessions
        .get_mut(session_id)
        .and_then(|session| session.servers.get_mut(server_id))
    {
        *phase = ServerPhase::Ready;
    }
}

pub(super) fn mark_failed(state: &Mutex<State>, session_id: &str, server_id: &str) {
    if let Some(phase) = state
        .lock()
        .sessions
        .get_mut(session_id)
        .and_then(|session| session.servers.get_mut(server_id))
    {
        *phase = ServerPhase::Failed;
    }
}
