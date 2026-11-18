//! 用户输入准入阶段的协议能力回归测试。
//!
//! 构造器复用父模块 `tests`；单独成文件以控制单文件规模。

use peri_acp::transport::AcpTransport;
use peri_acp::transport::types::IncomingMessage;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::session::{PluginSelection, UserInput, UserInputQueueSnapshot};
use serde_json::json;

use crate::kit::steer_state::SteerState;

use super::tests::RestoreProjection;
use super::{
    AcpTuiClient, AdmitOutcome, Duration, STEERS, SteerCommand, SteerCommandKind, SteerFailure,
    SteerStage, atoms,
};

/// 旧 Host 未回显 `peri.userInputQueue.pluginSelections`：带选择的入队必须在发送前
/// 拒绝，不能先发再等整条请求被 `deny_unknown_fields` 打回；完整原稿与选择回到
/// 可恢复草稿（R2）。
#[tokio::test]
#[serial_test::serial]
async fn test_admission_rejects_plugin_selections_without_negotiated_capability() {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    atoms::ACTIVE_SESSION_ID.set("s".into());
    atoms::BRIDGE_RESET_COUNTER.set(7);
    let mut projection = SteerState::default();
    projection.reset_session("s", 7);
    projection.accept_snapshot(
        UserInputQueueSnapshot {
            session_id: "s".into(),
            generation: "g".into(),
            revision: 1,
            active_request_id: None,
            items: vec![],
            terminal_items: vec![],
        },
        7,
        true,
    );
    let selections = vec![PluginSelection::new("alpha")];
    let mut command = SteerCommand {
        session_id: "s".into(),
        epoch: 7,
        command_id: "command".into(),
        generation: None,
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: "input".into(),
            original_draft: "选择插件后发送".into(),
            content: MessageContent::text("选择插件后发送"),
            plugin_selections: selections.clone(),
        }),
    };
    projection.begin(command.clone());
    STEERS.set(projection);
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s", false);

    let SteerFailure { error, stage } = super::execute(&client, &mut command, "/tmp")
        .await
        .expect_err("未协商 pluginSelections 时必须拒绝入队");
    assert_eq!(error.code, -32602, "能力缺失按请求错误拒绝");
    assert_eq!(stage, SteerStage::Admit, "拒绝发生在准入阶段");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.recv())
            .await
            .is_err(),
        "发送前拒绝不得产生任何 RPC"
    );
    assert!(
        super::reject_command(&mut command, &error),
        "能力拒绝必须按确定未受理结算"
    );
    let recovered = STEERS
        .state()
        .write()
        .recover("s", 7, true)
        .expect("明确拒绝必须恢复完整草稿");
    assert_eq!(
        recovered.original_draft, "选择插件后发送",
        "草稿原文必须保留"
    );
    assert_eq!(
        recovered.plugin_selections, selections,
        "插件选择必须随草稿保留"
    );
    client.close();
}

/// 已绑定 generation 的入队重试遇到本地能力降级：只能拒绝本次发送，
/// 不得把「原请求可能已受理」的未知结果结算成确定未受理并恢复草稿。
#[tokio::test]
#[serial_test::serial]
async fn test_capability_downgrade_keeps_bound_retry_unknown() {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    atoms::ACTIVE_SESSION_ID.set("s".into());
    atoms::BRIDGE_RESET_COUNTER.set(7);
    let snapshot = || UserInputQueueSnapshot {
        session_id: "s".into(),
        generation: "g".into(),
        revision: 1,
        active_request_id: None,
        items: vec![],
        terminal_items: vec![],
    };
    let mut projection = SteerState::default();
    projection.reset_session("s", 7);
    projection.accept_snapshot(snapshot(), 7, true);
    let selections = vec![PluginSelection::new("alpha")];
    let mut command = SteerCommand {
        session_id: "s".into(),
        epoch: 7,
        command_id: "uncertain-command".into(),
        generation: None,
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: uuid::Uuid::now_v7().to_string(),
            original_draft: "选择插件后发送".into(),
            content: MessageContent::text("选择插件后发送"),
            plugin_selections: selections.clone(),
        }),
    };
    projection.begin(command.clone());
    STEERS.set(projection);

    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let server_transport = std::sync::Arc::new(server_transport);
    let (client, notifications, _) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s", false);
    client.spawn_pump(notifications);
    client.force_plugin_selections_for_test(true);

    // 首次 enqueue 已发出，但回执丢失：结果未知且 generation 已绑定。
    let first_server = std::sync::Arc::clone(&server_transport);
    let first_response = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, params } = first_server
            .recv()
            .await
            .expect("首次 enqueue 必须到达服务端")
        else {
            panic!("应收到首次 enqueue");
        };
        assert_eq!(method, "session/input/enqueue");
        assert_eq!(params["pluginSelections"][0]["pluginId"], "alpha");
        first_server
            .send_response(id, Err(super::AcpError::new(-32603, "receipt unavailable")))
            .await
            .expect("首次回执必须送达");
    });
    let failure = super::execute(&client, &mut command, "/tmp")
        .await
        .expect_err("回执丢失必须返回错误");
    assert_eq!(failure.error.code, -32603);
    assert!(
        !super::reject_command(&mut command, &failure.error),
        "回执丢失仍是未知结果"
    );
    first_response.await.expect("首次交换必须完成");
    assert_eq!(command.generation.as_deref(), Some("g"));

    // 服务端能力降级后重试：本地闸门只拒绝本次发送，未知账本保持原样。
    client.force_plugin_selections_for_test(false);
    let failure = super::execute(&client, &mut command, "/tmp")
        .await
        .expect_err("能力缺失必须在发送前拒绝");
    assert_eq!(
        failure.error.code, -32603,
        "已绑定 generation 的重试不得被标记为确定拒绝"
    );
    assert_eq!(failure.stage, SteerStage::Admit);
    assert!(
        !super::reject_command(&mut command, &failure.error),
        "本地拒绝不得结算此前可能已受理的请求"
    );
    assert!(
        STEERS
            .state()
            .read()
            .pending_command("s", "uncertain-command")
            .is_some(),
        "未知结果命令必须保留在待结算账本"
    );
    assert!(
        STEERS.state().write().recover("s", 7, true).is_none(),
        "不得生成重复草稿"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server_transport.recv())
            .await
            .is_err(),
        "能力降级期间的重试不得产生 RPC"
    );

    // 能力恢复后重试同一命令：幂等入队并结算。
    client.force_plugin_selections_for_test(true);
    let recovery_server = std::sync::Arc::clone(&server_transport);
    let recovery_response = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, params } = recovery_server
            .recv()
            .await
            .expect("恢复后的重试必须到达服务端")
        else {
            panic!("应收到恢复后的 enqueue");
        };
        assert_eq!(method, "session/input/enqueue");
        assert_eq!(params["commandId"], "uncertain-command");
        assert_eq!(params["generation"], "g", "重试必须复用原实例绑定");
        recovery_server
            .send_response(
                id,
                Ok(json!({
                    "snapshot":{"sessionId":"s","generation":"g","revision":2,"items":[]},
                    "results":[]
                })),
            )
            .await
            .expect("恢复回执必须送达");
    });
    assert_eq!(
        super::execute(&client, &mut command, "/tmp")
            .await
            .expect("能力恢复后重试必须成功"),
        AdmitOutcome::Submitted
    );
    recovery_response.await.expect("恢复交换必须完成");
    assert!(
        STEERS
            .state()
            .read()
            .pending_command("s", "uncertain-command")
            .is_none(),
        "成功回执必须结算命令"
    );
    client.close();
}
