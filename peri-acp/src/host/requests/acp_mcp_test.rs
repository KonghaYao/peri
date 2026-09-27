//! MCP over ACP 宿主接线的契约测试。
//!
//! 三条接线各自钉一个协议事实，都不经过真实网络：
//!
//! 1. **声明解析**：`mcpServers` 的 wire 形状取自 SDK 自身的序列化结果，acp 型
//!    条目转成规格、其它传输与畸形条目被忽略且不牵连同批；
//! 2. **入站路由**：`mcp/message` 只带 `connectionId`，宿主必须在各会话服务间
//!    定位承载者，错误码按协议约定（`-32601`/`-32602` 之外用 `CODE_NOT_FOUND`）；
//! 3. **出站网关**：ACP 请求/通知原样透传，错误码不抹平——`mcp/message` 的码值
//!    就是内层 MCP 错误码，抹平会让 rmcp 的 lifecycle 协商不可用。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol_schema::v1::{McpServer, McpServerAcp, McpServerAcpId, McpServerHttp};
use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpServerSpec};
use peri_acp_types::ports::{AcpMcpGatewayPort, AcpMcpServerPort};
use serde_json::{json, Value};

use super::*;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};

/// 声明方的 wire 形状就是契约本身：这里由 SDK 的类型序列化产出，测试只断言
/// 解析器认的是这个形状（形状一旦漂移，解析器必须跟着改）。
fn acp_declaration(name: &str, server_id: &str) -> Value {
    serde_json::to_value(McpServer::Acp(McpServerAcp::new(
        name.to_string(),
        McpServerAcpId::new(server_id.to_string()),
    )))
    .unwrap()
}

#[test]
fn acp_declaration_wire_shape_is_the_sdk_shape() {
    assert_eq!(
        acp_declaration("fixture", "srv-1"),
        json!({"type": "acp", "name": "fixture", "serverId": "srv-1"})
    );
}

#[test]
fn acp_declarations_are_parsed_and_other_transports_ignored() {
    let http = serde_json::to_value(McpServer::Http(McpServerHttp::new(
        "remote",
        "https://example.invalid/mcp",
    )))
    .unwrap();
    let params = json!({
        "cwd": "/tmp",
        "mcpServers": [http, acp_declaration("fixture", "srv-1")],
    });

    assert_eq!(
        parse_acp_servers(&params, "s1"),
        vec![AcpMcpServerSpec {
            session_id: "s1".to_string(),
            name: "fixture".to_string(),
            server_id: "srv-1".to_string(),
        }]
    );
}

#[test]
fn malformed_declaration_drops_only_that_entry() {
    let raw =
        |name: &str, server_id: &str| json!({"type": "acp", "name": name, "serverId": server_id});
    let params = json!({
        "mcpServers": [
            // 未知传输形态：不属于本路径
            {"type": "carrier-pigeon", "name": "pigeon"},
            // 缺 serverId：无法路由回声明方，必须丢弃
            {"type": "acp", "name": "no-id"},
            {"type": "acp", "name": "empty-id", "serverId": ""},
            // 合法条目不受同批畸形影响
            raw("fixture", "srv-1"),
        ],
    });

    assert_eq!(
        parse_acp_servers(&params, "s1"),
        vec![AcpMcpServerSpec {
            session_id: "s1".to_string(),
            name: "fixture".to_string(),
            server_id: "srv-1".to_string(),
        }]
    );
}

#[test]
fn absent_or_non_array_declarations_are_no_declarations() {
    assert!(parse_acp_servers(&json!({"cwd": "/tmp"}), "s1").is_empty());
    assert!(parse_acp_servers(&json!({"mcpServers": null}), "s1").is_empty());
    assert!(parse_acp_servers(&json!({"mcpServers": "oops"}), "s1").is_empty());
    assert!(parse_acp_servers(&json!({"mcpServers": []}), "s1").is_empty());
}

/// 只承载一条连接的最小服务假件：路由只依 `owns_connection` 判定。
struct FakePort {
    owned: Vec<&'static str>,
}

impl FakePort {
    fn new(owned: &[&'static str]) -> Self {
        Self {
            owned: owned.to_vec(),
        }
    }
}

#[async_trait]
impl AcpMcpServerPort for FakePort {
    fn attach(&self, _gateway: Arc<dyn AcpMcpGatewayPort>, _servers: Vec<AcpMcpServerSpec>) {}

    async fn request(&self, _inbound: AcpMcpInbound) -> Result<Value, AcpMcpError> {
        Err(AcpMcpError::unavailable("测试假件不承载请求"))
    }

    async fn notify(&self, _inbound: AcpMcpInbound) -> Result<(), AcpMcpError> {
        Err(AcpMcpError::unavailable("测试假件不承载通知"))
    }

    fn owns_connection(&self, connection_id: &str) -> bool {
        self.owned.contains(&connection_id)
    }

    async fn close_session(&self, _session_id: &str) {}
}

#[test]
fn routing_picks_the_port_that_owns_the_connection() {
    let first: Arc<dyn AcpMcpServerPort> = Arc::new(FakePort::new(&["conn-a"]));
    let second: Arc<dyn AcpMcpServerPort> = Arc::new(FakePort::new(&["conn-b"]));
    let ports = vec![Arc::clone(&first), Arc::clone(&second)];

    let found = locate_owner(ports.iter().cloned(), "conn-b").expect("承载连接的服务");
    assert!(Arc::ptr_eq(&found, &second));
    assert!(locate_owner(ports.into_iter(), "conn-unknown").is_none());
}

#[test]
fn inbound_routing_rejects_malformed_and_unknown_connections() {
    let sessions = HashMap::new();
    let reject = |params: &Value| match route_inbound(&sessions, params) {
        Err(error) => error,
        Ok(_) => panic!("非法的 mcp/message 载荷不得路由成功: {params}"),
    };

    let missing_connection = reject(&json!({"method": "tools/list"}));
    assert_eq!(missing_connection.code, -32602);

    let missing_method = reject(&json!({"connectionId": "conn-1"}));
    assert_eq!(missing_method.code, -32602);

    // 未知连接用「资源不存在」而非内部错误：对端据此区分「连接已结束」与
    // 「本节点故障」，与内层 MCP 的 -32601 语义也不混同。
    let unknown = reject(&json!({"connectionId": "conn-1", "method": "tools/list"}));
    assert_eq!(unknown.code, AcpMcpError::CODE_NOT_FOUND);
}

/// 记录出站调用并可回错的 transport 假件（client 侧的最小协议对端）。
#[derive(Default)]
struct RecordingTransport {
    requests: Mutex<Vec<(String, Value)>>,
    notifications: Mutex<Vec<(String, Value)>>,
    reply_error: Option<AcpError>,
}

#[async_trait]
impl crate::transport::AcpTransport for RecordingTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.requests.lock().push((method.to_string(), params));
        match &self.reply_error {
            Some(error) => Err(error.clone()),
            None => Ok(json!({"connectionId": "conn-1"})),
        }
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications.lock().push((method.to_string(), params));
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

#[tokio::test]
async fn gateway_forwards_acp_requests_and_notifications() {
    let transport = Arc::new(RecordingTransport::default());
    let gateway =
        AcpTransportGateway::new(Arc::clone(&transport) as Arc<dyn crate::transport::AcpTransport>);

    let connected = gateway
        .request("mcp/connect", json!({"serverId": "srv-1"}))
        .await
        .unwrap();
    assert_eq!(connected, json!({"connectionId": "conn-1"}));
    assert_eq!(
        transport.requests.lock().as_slice(),
        &[("mcp/connect".to_string(), json!({"serverId": "srv-1"}))]
    );

    gateway
        .notify("mcp/message", json!({"connectionId": "conn-1"}))
        .await
        .unwrap();
    assert_eq!(transport.notifications.lock().len(), 1);
}

#[tokio::test]
async fn gateway_preserves_inner_mcp_error_codes() {
    let transport = Arc::new(RecordingTransport {
        // 内层 MCP 的方法级错误：码值必须原样到达桥接侧，rmcp 据此判断
        // legacy 回退而不是把连接判为故障。
        reply_error: Some(AcpError::new(-32601, "method not found: server/discover")),
        ..RecordingTransport::default()
    });
    let gateway =
        AcpTransportGateway::new(Arc::clone(&transport) as Arc<dyn crate::transport::AcpTransport>);

    let error = gateway
        .request("mcp/message", json!({"connectionId": "conn-1"}))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        AcpMcpError {
            code: -32601,
            message: "method not found: server/discover".to_string(),
        }
    );
}
