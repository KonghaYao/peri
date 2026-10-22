use super::*;
use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};
use serde_json::json;

/// 走一次真实 initialize 协商，返回写入 atom 的窗口能力。
///
/// 客户端在**本地 beta 开启**时以 `PeriCaps::all_enabled` 为基座显式声明请求能力；
/// 服务端能力事实只来自响应的 `agentCapabilities._meta.peri.historyWindowV1` 回显，
/// 未回显一律 fail-closed。
async fn negotiate_history_window(agent_capabilities: serde_json::Value) -> bool {
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
        assert_eq!(
            params["clientCapabilities"]["_meta"]["peri.historyWindowV1"],
            json!(true),
            "本地 beta 开启时 TUI 仍显式请求窗口能力（协商输入，不是能力事实）"
        );
        server_transport
            .send_response(id, Ok(json!({"agentCapabilities": agent_capabilities})))
            .await
            .unwrap();
    });
    crate::kit::atoms::HISTORY_WINDOW_V1.set(true);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    client.register_ui_commands(&[]).await.unwrap();
    server.await.unwrap();
    let capable = crate::kit::atoms::HISTORY_WINDOW_CAPABLE.get();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
    client.close();
    capable
}

/// 本地 beta 关闭时必须声明 `peri.historyWindowV1=false`。
///
/// host 以该声明判断「首屏由窗口 open 交付」，声明 true 却因本地开关不发 open
/// 会让 `session/load` 跳过回放，恢复会话首屏历史缺失（R01）。
#[tokio::test]
#[serial_test::serial]
async fn local_disabled_beta_does_not_declare_history_window() {
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
        assert_eq!(
            params["clientCapabilities"]["_meta"]["peri.historyWindowV1"],
            json!(false),
            "本地 beta 关闭时不得声明窗口能力，否则 host 会跳过历史回放"
        );
        server_transport
            .send_response(
                id,
                Ok(json!({
                    "agentCapabilities": {"_meta": {"peri.historyWindowV1": true}}
                })),
            )
            .await
            .unwrap();
    });
    crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    client.register_ui_commands(&[]).await.unwrap();
    server.await.unwrap();
    assert!(
        crate::kit::atoms::HISTORY_WINDOW_CAPABLE.get(),
        "服务端回显的事实仍应记录，是否走窗口由本地开关与能力共同决定"
    );
    assert!(
        !crate::kit::history_page_consumer::window_path_enabled(),
        "本地 beta 关闭时窗口路径必须保持关闭"
    );
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    client.close();
}

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
    assert!(
        !crate::kit::atoms::HISTORY_WINDOW_CAPABLE.get(),
        "initialize 未回显时必须 fail-closed"
    );
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    client.close();
}

/// 服务端能力事实只有显式回显 true 才算数：缺省/显式 false 都关闭。
#[tokio::test]
#[serial_test::serial]
async fn test_history_window_capability_requires_explicit_server_true() {
    assert!(
        !negotiate_history_window(json!({})).await,
        "未回显必须 fail-closed，不能继承客户端请求值"
    );
    assert!(
        !negotiate_history_window(json!({"_meta": {"peri.historyWindowV1": false}})).await,
        "显式 false 必须关闭"
    );
    assert!(
        negotiate_history_window(json!({"_meta": {"peri.historyWindowV1": true}})).await,
        "显式 true 才启用"
    );
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
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

/// H5a：V2 本地 beta 开启时只声明 `peri.history`，服务端明确选择后才启用；
/// 未回显时不建立 V2 能力（旧 V1 / 标准路径继续可用）。
#[tokio::test]
#[serial_test::serial]
async fn test_history_v2_declaration_requires_explicit_server_selection() {
    use peri_acp_types::history_v2::{HISTORY_META_KEY, HistoryLimits};

    crate::kit::atoms::HISTORY_V2.set(true);
    crate::kit::atoms::HISTORY_WINDOW_V1.set(true);
    crate::kit::atoms::HISTORY_V2_CAPABLE.set(false);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
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
            "V2 beta 开启时必须声明统一历史能力"
        );
        assert_eq!(
            meta["peri.historyWindowV1"],
            json!(false),
            "不得同时声明旧窗口能力（服务端会拒绝混合声明）"
        );
        assert_eq!(
            meta["peri.historyPagination"],
            json!(false),
            "不得同时声明旧分页能力"
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

    // 未回显：保持关闭，不回落到伪 V2。
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
        .expect_err("beta 开启而服务端未选择 V2 时必须终止启动");
    assert!(
        error.contains("History V2"),
        "错误必须明确指向 History V2：{error}"
    );

    crate::kit::atoms::HISTORY_V2.set(false);
    // 未发起 V2 请求的连接按启动配置（atom）取值；beta 关闭时启动准入不变。
    let (off_transport, off_server) = mpsc_transport_pair();
    let (off_client, _, _) = AcpTuiClient::new(off_transport);
    assert!(
        off_client.require_history_v2_startup().is_ok(),
        "beta 关闭时不改变旧启动路径"
    );
    off_client.close();
    drop(off_server);
    crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    client.close();
    client2.close();
}
