//! MCP over ACP 宿主接线的契约测试。
//!
//! 三条接线各自钉一个协议事实，都不经过真实网络：
//!
//! 1. **声明解析**：`mcpServers` 的 wire 形状取自 SDK 自身的序列化结果，acp 型
//!    条目转成规格、其它传输与畸形条目被忽略且不牵连同批；
//! 2. **入站路由**：`mcp/message` 只带 `serverId`，宿主必须在各会话服务间
//!    定位承载者，错误码按协议约定（`-32601`/`-32602` 之外用 `CODE_NOT_FOUND`）；
//! 3. **出站网关**：ACP 请求原样透传，错误码不抹平——`mcp/message` 的码值
//!    就是内层 MCP 错误码，抹平会让 rmcp 的 lifecycle 协商不可用。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol_schema::v1::{McpServer, McpServerAcp, McpServerAcpId, McpServerHttp};
use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::acp_mcp::AcpMcpError;
use peri_acp_types::ports::AcpMcpGatewayPort;
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

#[test]
fn connection_registry_rejects_conflicts_and_releases_only_owner() {
    let registry = AcpMcpRegistry::default();
    registry.claim("shared-server", "s1").unwrap();
    registry.claim("shared-server", "s1").unwrap();
    let params = json!({"mcpServers": [acp_declaration("fixture", "shared-server")]});
    assert_eq!(
        validate_declarations(&registry, &params, "s2")
            .unwrap_err()
            .code,
        AcpMcpError::CODE_UNAVAILABLE
    );
    assert_eq!(
        registry.claim("shared-server", "s2").unwrap_err().code,
        AcpMcpError::CODE_UNAVAILABLE
    );
    registry.claim("other-server", "s2").unwrap();
    registry.release_session("s2");
    registry.release_session("s2");
    assert_eq!(registry.owner("shared-server").as_deref(), Some("s1"));
    assert_eq!(registry.owner("other-server"), None);
    registry.release_session("s1");
    registry.claim("shared-server", "s2").unwrap();
}

#[test]
fn inbound_routing_rejects_malformed_and_unknown_servers() {
    let sessions = HashMap::new();
    let reject = |params: &Value| match route_inbound(&AcpMcpRegistry::default(), &sessions, params)
    {
        Err(error) => error,
        Ok(_) => panic!("非法的 mcp/message 载荷不得路由成功: {params}"),
    };

    let missing_server = reject(&json!({"requestId": "r-1", "method": "tools/list"}));
    assert_eq!(missing_server.code, -32602);

    let missing_request = reject(&json!({"serverId": "srv-1", "method": "tools/list"}));
    assert_eq!(missing_request.code, -32602);

    let missing_method = reject(&json!({"serverId": "srv-1", "requestId": "r-1"}));
    assert_eq!(missing_method.code, -32602);

    // 未知 server 用「资源不存在」而非内部错误：对端据此区分「声明已结束」与
    // 「本节点故障」，与内层 MCP 的 -32601 语义也不混同。
    let unknown = reject(&json!({
        "serverId": "srv-1",
        "requestId": "r-1",
        "method": "tools/list"
    }));
    assert_eq!(unknown.code, AcpMcpError::CODE_NOT_FOUND);
}

/// 记录出站调用并可回错的 transport 假件（client 侧的最小协议对端）。
#[derive(Default)]
struct RecordingTransport {
    requests: Mutex<Vec<(String, Value)>>,
    reply_error: Option<AcpError>,
}

#[async_trait]
impl crate::transport::AcpTransport for RecordingTransport {
    async fn send_cancellable_request(
        &self,
        method: &str,
        params: Value,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Value, AcpError> {
        self.send_request(method, params).await
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.requests.lock().push((method.to_string(), params));
        match &self.reply_error {
            Some(error) => Err(error.clone()),
            None => Ok(json!({"result": {"tools": []}})),
        }
    }

    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
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
async fn gateway_forwards_mcp_message_requests() {
    let transport = Arc::new(RecordingTransport::default());
    let gateway =
        AcpTransportGateway::new(Arc::clone(&transport) as Arc<dyn crate::transport::AcpTransport>);

    let connected = gateway
        .request(
            "mcp/message",
            json!({"serverId": "srv-1", "requestId": "r-1", "method": "tools/list"}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(connected, json!({"result": {"tools": []}}));
    assert_eq!(
        transport.requests.lock().as_slice(),
        &[(
            "mcp/message".to_string(),
            json!({"serverId": "srv-1", "requestId": "r-1", "method": "tools/list"})
        )]
    );
}

#[tokio::test]
async fn acp_mcp_gateway_cancel_targets_outer_request_and_settles_without_reply() {
    use crate::transport::mpsc::mpsc_transport_pair;
    use agent_client_protocol_schema::v1::{
        CancelRequestNotification, PROTOCOL_LEVEL_METHOD_NAMES,
    };
    let (client, server) = mpsc_transport_pair();
    let gateway = AcpTransportGateway::new(Arc::new(server));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let request_cancel = cancellation.clone();
    let request = tokio::spawn(async move {
        gateway
            .request(
                "mcp/message",
                json!({"serverId": "srv", "requestId": "peri-2", "method": "ping"}),
                request_cancel,
            )
            .await
    });
    let IncomingMessage::Request { id, params, .. } = client.recv().await.unwrap() else {
        panic!("应收到 ACP 请求")
    };
    assert_eq!(params["requestId"], "peri-2");
    cancellation.cancel();
    let IncomingMessage::Notification { method, params } =
        tokio::time::timeout(std::time::Duration::from_secs(3), client.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("应收到取消通知")
    };
    assert_eq!(method, PROTOCOL_LEVEL_METHOD_NAMES.cancel_request);
    let notification: CancelRequestNotification = serde_json::from_value(params).unwrap();
    assert_eq!(
        serde_json::to_value(notification.request_id).unwrap(),
        serde_json::to_value(id).unwrap()
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        -32800
    );
    client.close();
}

#[cfg(not(target_os = "emscripten"))]
#[tokio::test]
async fn acp_mcp_stdio_cancel_uses_official_envelope() {
    use agent_client_protocol_schema::v1::{
        CancelRequestNotification, PROTOCOL_LEVEL_METHOD_NAMES,
    };
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (reader, _client_writer) = tokio::io::duplex(4096);
    let (writer, client_reader) = tokio::io::duplex(4096);
    let transport = crate::transport::stdio::StdioTransport::from_reader_writer(reader, writer);
    let gateway = AcpTransportGateway::new(Arc::new(transport));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let request_cancel = cancellation.clone();
    let request = tokio::spawn(async move {
        gateway
            .request(
                "mcp/message",
                json!({"serverId": "srv", "requestId": "peri-7", "method": "ping"}),
                request_cancel,
            )
            .await
    });
    let mut lines = BufReader::new(client_reader).lines();
    let wire: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(wire["params"]["requestId"], "peri-7");
    cancellation.cancel();
    let cancel: Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(3), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(cancel.get("id").is_none());
    assert_eq!(cancel["method"], PROTOCOL_LEVEL_METHOD_NAMES.cancel_request);
    let notification: CancelRequestNotification =
        serde_json::from_value(cancel["params"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(notification.request_id).unwrap(),
        wire["id"]
    );
    assert_eq!(request.await.unwrap().unwrap_err().code, -32800);
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
        .request(
            "mcp/message",
            json!({"serverId": "srv-1", "requestId": "r-1", "method": "server/discover"}),
            tokio_util::sync::CancellationToken::new(),
        )
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
