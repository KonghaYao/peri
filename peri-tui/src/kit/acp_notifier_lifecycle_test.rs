use super::*;
use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::BufferedInput;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair};
use serde_json::json;
use serial_test::serial;
fn spawn_test_notifier() -> (
    mpsc::UnboundedSender<AcpNotification>,
    mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    CancellationToken,
) {
    let (notif_tx, notif_rx) = mpsc::unbounded_channel::<AcpNotification>();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_kit_notifier(notif_rx, bridge_tx, shutdown.clone());
    (notif_tx, bridge_rx, shutdown)
}

#[tokio::test]
async fn bridge_delivery_failure_claims_and_settles_registered_interaction() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s1", true);
    client.spawn_pump(notification_tx);

    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
    drop(bridge_rx);
    let shutdown = CancellationToken::new();
    let _notifier =
        spawn_kit_notifier_with_client(notification_rx, bridge_tx, shutdown.clone(), client);

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        server_transport.send_request(
            "session/request_permission",
            json!({"sessionId":"s1","toolCall":{"title":"Bash","rawInput":{}}}),
        ),
    )
    .await
    .expect("bridge rejection must not leave reverse request hanging")
    .expect("bridge rejection must settle the reverse request");
    let response: agent_client_protocol::schema::v1::RequestPermissionResponse =
        serde_json::from_value(response).unwrap();
    assert!(matches!(
        response.outcome,
        agent_client_protocol::schema::v1::RequestPermissionOutcome::Cancelled
    ));
    shutdown.cancel();
}

#[tokio::test]
async fn test_agent_done_forwards_turn_done_to_bridges() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "end_turn".into(),
            request_id: None,
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 TurnDone");
    assert!(matches!(bridge_event.event, AcpEventData::TurnDone));

    shutdown.cancel();
}

/// Issue 2026-08-05 返工：AgentDone(cancelled) 应透传 request_id 到
/// TurnInterrupted，供 bridge 的 stale 配对判定。
#[tokio::test]
async fn test_agent_done_cancelled_forwards_request_id_to_turn_interrupted() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "cancelled".into(),
            request_id: Some("rid-1".into()),
        })
        .unwrap();

    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收到 TurnInterrupted");
    match bridge_event.event {
        AcpEventData::TurnInterrupted { reason, request_id } => {
            assert_eq!(reason, "user cancelled");
            assert_eq!(request_id.as_deref(), Some("rid-1"), "request_id 应透传");
        }
        other => panic!("expected TurnInterrupted, got {other:?}"),
    }

    shutdown.cancel();
}

#[tokio::test]
async fn test_channel_close_exits_cleanly() {
    let (notif_tx, _bridge_rx, shutdown) = spawn_test_notifier();

    // 模拟 transport 断开：drop sender 让 recv() 返回 None
    drop(notif_tx);

    // 给任务一点时间退出
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // shutdown 仍可正常调用（任务已退出，cancel 信号无害）
    shutdown.cancel();
}

/// Issue 2026-08-05: transport 断开（notification channel 关闭）后 notifier
/// 兜底复位 loading/排队输入并提示断连——事件流中断后 is_loading 不再有事件
/// 驱动复位路径，否则 spinner 空转且 Ctrl+C 退出/命令全被 loading 门禁拦截。
///
/// 注意：本测试的单 sender 语义与生产 wiring 一致（Issue 2 重接后 sender 由
/// pump task 独占，pump 退出即 drop sender → channel 关闭）。
#[tokio::test]
#[serial]
async fn test_channel_close_resets_loading_and_input_buffer() {
    crate::kit::atoms::init_atoms();
    // 模拟卡死状态：is_loading=true + 排队输入
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = true;
    }
    let queued = BufferedInput::text("queued");
    INPUT_BUFFER.state().write().push_back(queued);
    *NOTIFICATION.state().write() = None;
    let hb_before = *RENDER_HEARTBEAT.state().read();

    let (notif_tx, _bridge_rx, shutdown) = spawn_test_notifier();
    // 模拟 transport 死亡：drop sender → notifier 的 recv() 返回 None
    drop(notif_tx);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(
        !ACP_STATE.state().read().is_loading,
        "channel 关闭后 is_loading 应复位为 false"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队输入应随事件流中断被清空"
    );
    assert!(
        NOTIFICATION.state().read().is_some(),
        "应提示断连（app-agent-disconnected）"
    );
    assert!(
        *RENDER_HEARTBEAT.state().read() > hb_before,
        "断连提示应触发重渲染心跳"
    );
    shutdown.cancel();
}

/// Issue 2 重接：真实生产 wiring（`AcpTuiClient::new` + `spawn_pump` +
/// `spawn_kit_notifier`）下，server transport 死亡 → pump 退出 → channel
/// 关闭 → notifier 兜底复位。
///
/// 实施前（v1）此测试失败（client struct 持有永活 sender，channel 不随 pump
/// 退出关闭）；实施后通过——同时是本方案的防回归守卫（任何把 sender 加回
/// struct/全局的改动都会使其失败）。
#[tokio::test]
#[serial]
async fn test_real_wiring_transport_death_resets_loading() {
    crate::kit::atoms::init_atoms();
    // 模拟卡死状态：is_loading=true + 排队输入
    ACP_STATE.state().write().is_loading = true;
    let queued = BufferedInput::text("queued");
    INPUT_BUFFER.state().write().push_back(queued);
    *NOTIFICATION.state().write() = None;
    let hb_before = *RENDER_HEARTBEAT.state().read();

    // 生产 wiring：client + pump + notifier（bridge 用 dummy rx 即可）
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let (bridge_tx, _bridge_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let _h = spawn_kit_notifier(notification_rx, bridge_tx, shutdown.clone());

    // 模拟生产 wiring 的额外 client 克隆（ACP_CLIENT_HANDLE / consumer / panel）
    let extra_clone = client.clone();
    drop(extra_clone);

    // 模拟 server task 死亡：drop server transport
    drop(server_transport);

    // 等待 pump 退出 → channel 关闭 → notifier 兜底
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert!(
        !ACP_STATE.state().read().is_loading,
        "transport 死亡后 is_loading 应复位"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队输入应随事件流中断被清空"
    );
    assert!(
        NOTIFICATION.state().read().is_some(),
        "应提示断连（app-agent-disconnected）"
    );
    assert!(
        *RENDER_HEARTBEAT.state().read() > hb_before,
        "断连提示应触发重渲染心跳"
    );
    shutdown.cancel();
}
