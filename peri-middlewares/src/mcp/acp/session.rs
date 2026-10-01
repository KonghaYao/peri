//! 会话级 MCP over ACP 连接管理。
//!
//! 一个 [`AcpMcpService`] 服务一条 ACP 连接上的全部会话：`attach` 登记 client
//! 在会话 setup 中声明的 acp 型 server 并后台建连；client 经 `mcp/message`
//! 反向下发的请求 / 通知按 `connectionId` 路由进对应 rmcp 连接；
//! `close_session` 断开该会话的全部连接。
//!
//! 建连顺序（与配置型 MCP 的既有链路同构）：
//! `mcp/connect` → 注册入站路由 → rmcp 握手（`serve_client_auto`）→
//! 工具发现（`tools/list`）→ `retain_service` → `commit_acp_connection`。
//!
//! 两条不变量：
//! 1. **不阻塞会话建立**：`attach` 只登记与派发任务，握手与发现在后台任务里
//!    有界完成，失败留在池状态面（`ClientStatus::Failed`）而不回抛给调用方；
//! 2. **不跨会话泄漏**：连接在池中登记 `acp_owners` 归属，工具桥接、发现面与
//!    状态面按归属会话过滤（`McpClientPool::is_visible_to_session`）。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpServerSpec};
use peri_acp_types::plugin::ConfigSource;
use peri_acp_types::ports::{AcpMcpGatewayPort, AcpMcpServerPort};
use serde_json::{Map, Value};

use super::transport::{create_bridge, AcpBridgeHandle, MCP_CONNECT_METHOD, MCP_DISCONNECT_METHOD};
use crate::mcp::client::{
    peer_declares_skills, redact_mcp_error, serve_client_auto, ClientStatus, McpClientHandle,
    McpClientPool, OAuthStatus, HTTP_CONNECT_TIMEOUT, SHUTDOWN_TIMEOUT,
};
use crate::mcp::task_scope::McpTaskKey;

/// 会话级 MCP over ACP 服务（`AcpMcpServerPort` 实现）。
///
/// deployment 级单例：构造点持具体类型，host 侧只持
/// `Arc<dyn AcpMcpServerPort>`。内部状态（会话 → 声明 → 连接）与 MCP 池
/// 分离，池只承担「已建立连接」的登记与工具面投影。
pub struct AcpMcpService {
    pool: Arc<McpClientPool>,
    state: Arc<Mutex<State>>,
}

/// 服务内部状态：会话声明与活跃连接索引。
///
/// `sessions` 用 `BTreeMap` 固定遍历顺序（建连派发顺序可复现）；`connections`
/// 是 `connectionId` → 桥接句柄的反向索引，入站 `mcp/message` 依赖它路由。
#[derive(Default)]
struct State {
    sessions: BTreeMap<String, SessionRecord>,
    connections: HashMap<String, ConnectionRecord>,
}

struct SessionRecord {
    gateway: Arc<dyn AcpMcpGatewayPort>,
    /// `serverId` → 建连状态（同一会话内 `serverId` 唯一）。
    servers: BTreeMap<String, ServerPhase>,
}

/// 单个声明的建连状态。
///
/// 失败原因不在本状态机里留存：事实源是 MCP 池（`record_acp_failure`），
/// 本枚举只回答「是否需要再建一次连接」。
enum ServerPhase {
    Connecting,
    Ready,
    Failed,
}

struct ConnectionRecord {
    session_id: String,
    server_id: String,
    handle: Arc<AcpBridgeHandle>,
}

impl AcpMcpService {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            pool,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// 登记一条声明；返回 `None` 表示无需建连（已在建 / 已就绪的幂等重复）。
    fn dispatch(
        &self,
        gateway: Arc<dyn AcpMcpGatewayPort>,
        spec: AcpMcpServerSpec,
    ) -> Option<AcpMcpServerSpec> {
        let mut state = self.state.lock();
        let record = state
            .sessions
            .entry(spec.session_id.clone())
            .or_insert_with(|| SessionRecord {
                gateway: Arc::clone(&gateway),
                servers: BTreeMap::new(),
            });
        match record.servers.get(&spec.server_id) {
            Some(ServerPhase::Connecting | ServerPhase::Ready) => None,
            // 失败过的声明允许重试（client 可在后续 session setup 中重发）。
            Some(ServerPhase::Failed) | None => {
                record
                    .servers
                    .insert(spec.server_id.clone(), ServerPhase::Connecting);
                Some(spec)
            }
        }
    }

    /// 建连任务的派发壳：任务本身失败不回抛，只记状态。
    fn spawn_connect(
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
            connect_server(task_pool, Arc::clone(&state), gateway, spec),
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

    /// `connectionId` → 桥接句柄。
    fn route(&self, connection_id: &str) -> Result<Arc<AcpBridgeHandle>, AcpMcpError> {
        self.state
            .lock()
            .connections
            .get(connection_id)
            .map(|connection| Arc::clone(&connection.handle))
            .ok_or_else(|| {
                AcpMcpError::not_found(format!("未知或已关闭的 MCP-over-ACP 连接: {connection_id}"))
            })
    }
}

#[async_trait::async_trait]
impl AcpMcpServerPort for AcpMcpService {
    fn attach(&self, gateway: Arc<dyn AcpMcpGatewayPort>, servers: Vec<AcpMcpServerSpec>) {
        for spec in servers {
            let Some(spec) = self.dispatch(Arc::clone(&gateway), spec) else {
                continue;
            };
            Self::spawn_connect(
                Arc::clone(&self.pool),
                Arc::clone(&self.state),
                Arc::clone(&gateway),
                spec,
            );
        }
    }

    async fn request(&self, inbound: AcpMcpInbound) -> Result<Value, AcpMcpError> {
        let handle = self.route(&inbound.connection_id)?;
        handle
            .request(&inbound.method, inbound.params.map(Value::Object))
            .await
            .map_err(|error| {
                // 内层 MCP 错误原样透传：请求方就是这台 server 的宿主机。
                AcpMcpError {
                    code: i64::from(error.code.0),
                    message: error.message.to_string(),
                }
            })
    }

    async fn notify(&self, inbound: AcpMcpInbound) -> Result<(), AcpMcpError> {
        let handle = self.route(&inbound.connection_id)?;
        handle
            .notify(&inbound.method, inbound.params.map(Value::Object))
            .map_err(|error| AcpMcpError::unavailable(error.message.to_string()))
    }

    fn owns_connection(&self, connection_id: &str) -> bool {
        self.state.lock().connections.contains_key(connection_id)
    }

    async fn close_session(&self, session_id: &str) {
        // 顺序固定：先摘会话与连接索引（建连任务的存活检查据此判定），再终止
        // 在建任务，最后移除池条目——被 abort 的任务不会在此之后提交。
        let (gateway, server_ids, connections) = {
            let mut state = self.state.lock();
            let Some(record) = state.sessions.remove(session_id) else {
                return;
            };
            let connections: Vec<(String, String, Arc<AcpBridgeHandle>)> = state
                .connections
                .iter()
                .filter(|(_, connection)| connection.session_id == session_id)
                .map(|(connection_id, connection)| {
                    (
                        connection_id.clone(),
                        connection.server_id.clone(),
                        Arc::clone(&connection.handle),
                    )
                })
                .collect();
            for (connection_id, _, _) in &connections {
                state.connections.remove(connection_id);
            }
            (
                record.gateway,
                record.servers.into_keys().collect::<Vec<_>>(),
                connections,
            )
        };

        for server_id in server_ids {
            self.pool
                .stop_background(&McpTaskKey::Acp {
                    session_id: session_id.to_string(),
                    server_id,
                })
                .await;
        }

        for (connection_id, server_id, handle) in connections {
            handle.close();
            disconnect(&gateway, &connection_id).await;
            tracing::debug!(
                session_id = %session_id,
                server_id = %server_id,
                "MCP over ACP 连接已断开"
            );
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

/// 建连任务主体：`mcp/connect` → 桥接 → rmcp 握手 → 工具发现 → 池提交。
///
/// 任何失败路径都必须成对收尾（注销入站路由 + `mcp/disconnect`），否则 client
/// 侧会留下一条无人使用的连接。
async fn connect_server(
    pool: Arc<McpClientPool>,
    state: Arc<Mutex<State>>,
    gateway: Arc<dyn AcpMcpGatewayPort>,
    spec: AcpMcpServerSpec,
) {
    let connection_id = match connect(&gateway, &spec.server_id).await {
        Ok(connection_id) => connection_id,
        Err(error) => {
            tracing::warn!(
                session_id = %spec.session_id,
                server = %spec.name,
                %error,
                "MCP over ACP 建连失败"
            );
            record_failure(
                &pool,
                &state,
                &spec.session_id,
                &spec.server_id,
                &preferred_name(&spec),
                &error.message,
            );
            return;
        }
    };

    let (transport, handle, runner) = create_bridge(Arc::clone(&gateway), connection_id.clone());
    tokio::spawn(runner.run());
    // 握手期间 client 侧 server 就可能下发通知，入站路由必须早于握手注册。
    if !register_connection(&state, &spec, &connection_id, Arc::clone(&handle)) {
        handle.close();
        disconnect(&gateway, &connection_id).await;
        return;
    }

    let served = serve_client_auto(
        transport,
        None,
        &pool.capability_profile,
        HTTP_CONNECT_TIMEOUT,
    )
    .await;
    let service = match served {
        Ok(Ok(service)) => service,
        Ok(Err(error)) => {
            let message = redact_mcp_error(&error.to_string());
            fail_connection(&pool, &state, &gateway, &spec, &connection_id, message).await;
            return;
        }
        Err(_) => {
            fail_connection(
                &pool,
                &state,
                &gateway,
                &spec,
                &connection_id,
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
            let message = redact_mcp_error(&error.to_string());
            let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
            fail_connection(&pool, &state, &gateway, &spec, &connection_id, message).await;
            return;
        }
    };
    if !connection_is_live(&state, &connection_id) {
        // 会话在握手 / 发现期间关闭：连接不落池，立即断开。
        let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        disconnect(&gateway, &connection_id).await;
        return;
    }

    let client = McpClientHandle {
        name: preferred_name(&spec),
        version: peer.peer_info().and_then(|info| {
            info.server_info
                .as_ref()
                .map(|server| server.version.clone())
        }),
        // 会话级连接的来源是声明它的 client，不参与持久缓存。
        cache_version: None,
        skills_capable: peer_declares_skills(&peer),
        peer: Some(peer),
        resources: Vec::new(),
        tools,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: Some(ConfigSource::Acp),
        url: None,
    };
    match pool.commit_acp_connection(&spec.session_id, &spec.name, Arc::new(client), service) {
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
            unregister_connection(&state, &connection_id);
            disconnect(&gateway, &connection_id).await;
        }
    }
}

/// 池内名：client 声明名为空时退回 `serverId`（工具命名要求非空前缀）。
fn preferred_name(spec: &AcpMcpServerSpec) -> String {
    let name = spec.name.trim();
    if name.is_empty() {
        spec.server_id.clone()
    } else {
        name.to_string()
    }
}

/// `mcp/connect { serverId }` → `connectionId`。
async fn connect(
    gateway: &Arc<dyn AcpMcpGatewayPort>,
    server_id: &str,
) -> Result<String, AcpMcpError> {
    let mut params = Map::new();
    params.insert("serverId".to_string(), Value::String(server_id.to_string()));
    let response = gateway
        .request(MCP_CONNECT_METHOD, Value::Object(params))
        .await?;
    response
        .get("connectionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AcpMcpError::unavailable("mcp/connect 响应缺少 connectionId"))
}

/// `mcp/disconnect { connectionId }`；失败只告警——连接已在本地拆除，对端延迟
/// 回收不改变本侧终态。
///
/// 等待有上界：会话关闭路径会 await 这里，而 transport 层没有内建超时，静默的
/// 对端不得把会话关闭拖成无限等待。
async fn disconnect(gateway: &Arc<dyn AcpMcpGatewayPort>, connection_id: &str) {
    let mut params = Map::new();
    params.insert(
        "connectionId".to_string(),
        Value::String(connection_id.to_string()),
    );
    let outcome = tokio::time::timeout(
        SHUTDOWN_TIMEOUT,
        gateway.request(MCP_DISCONNECT_METHOD, Value::Object(params)),
    )
    .await;
    match outcome {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::debug!(
            connection_id = %connection_id,
            %error,
            "mcp/disconnect 未收到对端确认"
        ),
        Err(_) => tracing::debug!(
            connection_id = %connection_id,
            timeout_secs = SHUTDOWN_TIMEOUT.as_secs(),
            "mcp/disconnect 等待对端确认超时"
        ),
    }
}

/// 注册入站路由；返回 false 表示会话已关闭（调用方必须立刻断开）。
fn register_connection(
    state: &Mutex<State>,
    spec: &AcpMcpServerSpec,
    connection_id: &str,
    handle: Arc<AcpBridgeHandle>,
) -> bool {
    let mut state = state.lock();
    if !state.sessions.contains_key(&spec.session_id) {
        return false;
    }
    state.connections.insert(
        connection_id.to_string(),
        ConnectionRecord {
            session_id: spec.session_id.clone(),
            server_id: spec.server_id.clone(),
            handle,
        },
    );
    true
}

fn unregister_connection(state: &Mutex<State>, connection_id: &str) -> bool {
    state.lock().connections.remove(connection_id).is_some()
}

/// 连接是否仍属于存活会话（会话关闭后不得提交进池）。
fn connection_is_live(state: &Mutex<State>, connection_id: &str) -> bool {
    state.lock().connections.contains_key(connection_id)
}

/// 失败收口：注销路由、`mcp/disconnect`、池状态面记失败。
async fn fail_connection(
    pool: &Arc<McpClientPool>,
    state: &Mutex<State>,
    gateway: &Arc<dyn AcpMcpGatewayPort>,
    spec: &AcpMcpServerSpec,
    connection_id: &str,
    message: String,
) {
    tracing::warn!(
        session_id = %spec.session_id,
        server = %spec.name,
        error = %message,
        "MCP over ACP 连接失败"
    );
    unregister_connection(state, connection_id);
    disconnect(gateway, connection_id).await;
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
fn record_failure(
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

fn mark_ready(state: &Mutex<State>, session_id: &str, server_id: &str) {
    if let Some(phase) = state
        .lock()
        .sessions
        .get_mut(session_id)
        .and_then(|session| session.servers.get_mut(server_id))
    {
        *phase = ServerPhase::Ready;
    }
}

fn mark_failed(state: &Mutex<State>, session_id: &str, server_id: &str) {
    if let Some(phase) = state
        .lock()
        .sessions
        .get_mut(session_id)
        .and_then(|session| session.servers.get_mut(server_id))
    {
        *phase = ServerPhase::Failed;
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
