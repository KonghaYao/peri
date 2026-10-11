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
