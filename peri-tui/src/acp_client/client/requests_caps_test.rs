use super::*;
use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};
use serde_json::json;

/// TUI 的 MCP OAuth 走 legacy 契约：initialize 必须关闭 `peri.oauth`。
///
/// 声明 oauth=true 会让 host 请求面切到 safe 契约——`mcp/oauth_start` 强制
/// 要求 flow_id（TUI 不发）、`mcp/oauth_callback` 拒绝授权码回传——[ 授权 ]
/// 按钮因此静默失败。同时 `peri.agentEvent` 必须保留（OauthNeeded 等
/// legacy 事件通道）。
#[tokio::test]
#[serial_test::serial]
async fn test_ui_capability_negotiation_keeps_legacy_oauth_contract() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, params } =
            server_transport.recv().await.unwrap()
        else {
            panic!("应收到 initialize");
        };
        assert_eq!(method, "initialize", "应先协商能力");
        let meta = &params["clientCapabilities"]["_meta"];
        assert_eq!(
            meta["peri.oauth"],
            json!(false),
            "TUI 不声明 peri.oauth 专用通道"
        );
        assert_eq!(
            meta["peri.agentEvent"],
            json!(true),
            "legacy 事件通道必须保留"
        );
        assert!(
            meta["peri.uiCommands"].is_array(),
            "ui 命令明细须随协商上送"
        );
        server_transport
            .send_response(id, Ok(json!({"agentCapabilities": {}})))
            .await
            .unwrap();
    });
    client.register_ui_commands(&[]).await.unwrap();
    server.await.unwrap();
    client.close();
}

#[tokio::test]
#[serial_test::serial]
async fn test_mcp_control_requires_server_true_and_resets_on_close() {
    for advertised in [json!(null), json!(false), json!("true"), json!(true)] {
        let (transport, server) = mpsc_transport_pair();
        let (client, _, _) = AcpTuiClient::new(transport);
        assert!(!client.supports_mcp_control(), "initialize 前禁止控制");
        let supported = advertised == json!(true);
        let server_task = tokio::spawn(async move {
            let Some(IncomingMessage::Request { id, method, params }) = server.recv().await else {
                panic!("应收到 initialize");
            };
            assert_eq!(method, "initialize");
            assert_eq!(
                params["clientCapabilities"]["_meta"]["peri.mcpControlV1"],
                json!(true)
            );
            server
                .send_response(
                    id,
                    Ok(json!({"agentCapabilities":{"_meta":{"peri.mcpControlV1":advertised}}})),
                )
                .await
                .unwrap();
        });
        client.register_ui_commands(&[]).await.unwrap();
        assert_eq!(
            client.supports_mcp_control(),
            supported,
            "能力只认服务端布尔 true"
        );
        client.close();
        assert!(!client.supports_mcp_control(), "连接关闭后清除能力");
        server_task.await.unwrap();
    }
}

/// TUI 始终只声明 History V2；旧私有能力必须在 TUI 侧恒为 false。
/// 服务端明确选择 version=2/windowed 后才启用。
#[tokio::test]
#[serial_test::serial]
async fn test_history_v2_declaration_requires_explicit_server_selection() {
    use peri_acp_types::history_v2::{HISTORY_META_KEY, HistoryLimits};

    crate::kit::atoms::HISTORY_V2_CAPABLE.set(false);
    let limits = HistoryLimits::default();
    let server_capability = json!({
        "version": 2,
        "selectedProfile": "windowed",
        "method": "_peri/history",
        "notification": "_peri/history/changed",
        "limits": &limits,
    });
    let response_meta = json!({"peri.history": server_capability});

    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, params } =
            server_transport.recv().await.unwrap()
        else {
            panic!("应收到 initialize");
        };
        assert_eq!(method, "initialize");
        let meta = &params["clientCapabilities"]["_meta"];
        assert_eq!(
            meta[HISTORY_META_KEY],
            json!({"versions": [2], "requestedProfile": "windowed"}),
            "TUI 必须声明统一历史能力"
        );
        assert!(
            meta.get("peri.historyWindowV1").is_none(),
            "不得声明旧窗口能力"
        );
        assert!(
            meta.get("peri.historyPagination").is_none(),
            "不得声明旧分页能力"
        );
        server_transport
            .send_response(
                id,
                Ok(json!({"agentCapabilities": {"_meta": response_meta}})),
            )
            .await
            .unwrap();
    });
    client.register_ui_commands(&[]).await.unwrap();
    server.await.unwrap();
    assert!(
        crate::kit::atoms::HISTORY_V2_CAPABLE.get(),
        "服务端明确选择 version=2/windowed 后才启用"
    );
    assert!(
        client.require_history_v2_startup().is_ok(),
        "服务端选择 V2 后启动准入通过"
    );
    assert_eq!(
        client.history_v2_limits(),
        limits,
        "协商预算必须写入客户端事实源"
    );

    // 未回显：保持关闭，不回落到旧协议或伪 V2。
    crate::kit::atoms::HISTORY_V2_CAPABLE.set(false);
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client2, notification_tx, _) = AcpTuiClient::new(client_transport);
    client2.spawn_pump(notification_tx);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, .. } = server_transport.recv().await.unwrap()
        else {
            panic!("应收到 initialize");
        };
        assert_eq!(method, "initialize");
        server_transport
            .send_response(id, Ok(json!({"agentCapabilities": {}})))
            .await
            .unwrap();
    });
    client2.register_ui_commands(&[]).await.unwrap();
    server.await.unwrap();
    assert!(
        !crate::kit::atoms::HISTORY_V2_CAPABLE.get(),
        "服务端未选择 V2 时必须 fail-closed"
    );
    let error = client2
        .require_history_v2_startup()
        .expect_err("服务端未选择 V2 时必须终止启动");
    assert!(
        error.contains("History V2"),
        "错误必须明确指向 History V2：{error}"
    );

    client.close();
    client2.close();
}

/// 服务端回显旧私有能力不能使启动通过：V2 是唯一路径。
#[tokio::test]
#[serial_test::serial]
async fn test_legacy_history_capabilities_never_satisfy_startup() {
    crate::kit::atoms::HISTORY_V2_CAPABLE.set(false);
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, .. } = server_transport.recv().await.unwrap()
        else {
            panic!("应收到 initialize");
        };
        assert_eq!(method, "initialize");
        server_transport
            .send_response(
                id,
                Ok(json!({
                    "agentCapabilities": {"_meta": {
                        "peri.historyWindowV1": true,
                        "peri.historyPagination": true,
                    }}
                })),
            )
            .await
            .unwrap();
    });
    client.register_ui_commands(&[]).await.unwrap();
    server.await.unwrap();
    assert!(!crate::kit::atoms::HISTORY_V2_CAPABLE.get());
    assert!(
        client.require_history_v2_startup().is_err(),
        "旧私有能力不得满足 V2 启动准入"
    );
    client.close();
}
