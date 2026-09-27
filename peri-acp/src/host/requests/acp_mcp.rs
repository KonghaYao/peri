//! MCP over ACP（client 声明的 `type: "acp"` server）的宿主接线。
//!
//! 三个方向各自一段：
//!
//! - **声明解析**：会话 setup（`session/new|load|resume|fork`）的 `mcpServers`
//!   里筛出 acp 型条目，转成契约规格交给该会话的服务
//!   （[`attach_session_servers`]，由 `ServerLoop` 在 setup 响应写入后调用）；
//! - **入站消息**：client 经 `mcp/message` 反向下发的请求 / 通知按
//!   `connectionId` 定位承载它的会话服务（[`route_inbound`]），请求走
//!   `ServerLoop` 的 spawn 路径、通知在通知分发里就地转发；
//! - **出站网关**：[`AcpTransportGateway`] 把服务的 `mcp/connect` /
//!   `mcp/message` / `mcp/disconnect` 落成 ACP 请求 / 通知。
//!
//! 会话服务是**会话级**的（每个会话各持一个 MCP 池，ACP 连接进的是本会话的
//! 工具面），而 `mcp/message` 只带 `connectionId`：定位承载者是宿主职责，见
//! [`AcpMcpServerPort::owns_connection`]。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol_schema::v1::McpServer;
use peri_acp_types::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpServerSpec};
use peri_acp_types::ports::{AcpMcpGatewayPort, AcpMcpServerPort};
use serde_json::Value;
use tracing::{debug, warn};

use super::super::{AcpServerConfig, SessionState};
use crate::transport::{types::AcpError, AcpTransport};

/// agent → client 的 ACP 发送网关：MCP over ACP 的全部出站消息经它落成
/// `mcp/connect` / `mcp/message` / `mcp/disconnect` 请求或通知。
///
/// 构造点在 setup 响应写入之后（`ServerLoop` 同时持有会话与
/// `Arc<dyn AcpTransport>` 的位置）；`mcp/message` 的错误码按协议约定就是内层
/// MCP 错误码，原样透传。
pub(crate) struct AcpTransportGateway {
    transport: Arc<dyn AcpTransport>,
}

impl AcpTransportGateway {
    pub(crate) fn new(transport: Arc<dyn AcpTransport>) -> Self {
        Self { transport }
    }
}

#[async_trait::async_trait]
impl AcpMcpGatewayPort for AcpTransportGateway {
    async fn request(&self, method: &str, params: Value) -> Result<Value, AcpMcpError> {
        self.transport
            .send_request(method, params)
            .await
            .map_err(transport_error)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), AcpMcpError> {
        self.transport
            .send_notification(method, params)
            .await
            .map_err(transport_error)
    }
}

fn transport_error(error: AcpError) -> AcpMcpError {
    AcpMcpError {
        code: error.code,
        message: error.message,
    }
}

/// 从会话 setup 参数里筛出 acp 型 MCP 声明。
///
/// 其它传输形态（stdio / http / sse）不归本路径：它们是 agent 自行启动的外部
/// 进程 / 端点，与 ACP 通道无关。
///
/// 逐条解析：单条畸形（未知传输形态、缺 `serverId`、无法反序列化）只丢该条并记
/// 日志，不牵连同批合法的 server——声明是 client 的输入，一条坏声明不该让整批
/// server 静默消失。
fn parse_acp_servers(params: &Value, session_id: &str) -> Vec<AcpMcpServerSpec> {
    let Some(raw) = params.get("mcpServers") else {
        return Vec::new();
    };
    if raw.is_null() {
        return Vec::new();
    }
    let Some(entries) = raw.as_array() else {
        warn!(
            session_id = %session_id,
            "session setup 的 mcpServers 不是数组，按未声明处理"
        );
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let server: McpServer = match serde_json::from_value(entry.clone()) {
                Ok(server) => server,
                Err(error) => {
                    warn!(
                        session_id = %session_id,
                        %error,
                        "mcpServers 条目无法解析，忽略该条"
                    );
                    return None;
                }
            };
            let McpServer::Acp(acp) = server else {
                return None;
            };
            let server_id = acp.server_id.to_string();
            if server_id.is_empty() {
                warn!(session_id = %session_id, "mcpServers 的 acp 声明缺少 serverId，忽略");
                return None;
            }
            Some(AcpMcpServerSpec {
                session_id: session_id.to_string(),
                name: acp.name,
                server_id,
            })
        })
        .collect()
}

/// 登记本次会话 setup 声明的 acp 型 server（非阻塞：只登记并后台建连）。
///
/// 由 `ServerLoop` 在 setup 响应成功写入后调用：客户端此时已拿到会话结果，
/// `mcp/connect` 只带 client 自己声明的 `serverId`，顺序上先响应再建连，客户端
/// 不必处理「会话还没告诉我就要求连 MCP」的交错。
///
/// 未装配 MCP over ACP 服务的会话（无工作区资源 / bare / print 模式）是无操作：
/// 这些会话没有 MCP 池可承载连接，声明无处落地。
pub(crate) fn attach_session_servers(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn AcpTransport>,
    params: &Value,
    session_id: &str,
) {
    let Some(port) = cfg.acp_mcp.as_ref() else {
        return;
    };
    let servers = parse_acp_servers(params, session_id);
    if servers.is_empty() {
        return;
    }
    debug!(
        session_id = %session_id,
        servers = servers.len(),
        "MCP over ACP 声明已受理，后台建连"
    );
    port.attach(
        Arc::new(AcpTransportGateway::new(Arc::clone(transport))),
        servers,
    );
}

/// 定位承载 `connectionId` 的会话服务（连接与会话的事实都在会话级服务里）。
///
/// 只依 `owns_connection` 判定，不关心端口从哪来：会话集合的形态是宿主细节，
/// 判定规则是协议事实。
fn locate_owner(
    ports: impl Iterator<Item = Arc<dyn AcpMcpServerPort>>,
    connection_id: &str,
) -> Option<Arc<dyn AcpMcpServerPort>> {
    ports
        .into_iter()
        .find(|port| port.owns_connection(connection_id))
}

/// 反序列化入站 `mcp/message` 载荷。
fn inbound(params: &Value) -> Result<AcpMcpInbound, AcpError> {
    let connection_id = params
        .get("connectionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AcpError::new(-32602, "missing connectionId"))?;
    let method = params
        .get("method")
        .and_then(Value::as_str)
        .filter(|method| !method.is_empty())
        .ok_or_else(|| AcpError::new(-32602, "missing method"))?
        .to_string();
    let params = match params.get("params") {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => Some(map.clone()),
        Some(_) => return Err(AcpError::new(-32602, "params must be an object")),
    };
    Ok(AcpMcpInbound {
        connection_id: connection_id.to_string(),
        method,
        params,
    })
}

/// 入站 `mcp/message` 的宿主路由：解析载荷 + 定位承载会话的服务。
///
/// 返回**已克隆的服务句柄**，调用方随即释放会话锁再 await——内层处理时长由
/// 对端（client 宿主的 MCP server）决定，不得占着会话锁等待。
pub(crate) fn route_inbound(
    sessions: &HashMap<String, SessionState>,
    params: &Value,
) -> Result<(Arc<dyn AcpMcpServerPort>, AcpMcpInbound), AcpError> {
    let inbound = inbound(params)?;
    let ports = sessions.values().filter_map(|state| {
        state
            .environment
            .as_ref()?
            .cfg
            .acp_mcp
            .as_ref()
            .map(Arc::clone)
    });
    let Some(port) = locate_owner(ports, &inbound.connection_id) else {
        return Err(AcpError::new(
            AcpMcpError::CODE_NOT_FOUND,
            format!(
                "未知或已关闭的 MCP-over-ACP 连接: {}",
                inbound.connection_id
            ),
        ));
    };
    Ok((port, inbound))
}

#[cfg(test)]
#[path = "acp_mcp_test.rs"]
mod tests;
