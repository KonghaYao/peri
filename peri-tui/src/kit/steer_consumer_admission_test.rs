//! 用户输入准入阶段的协议能力回归测试。
//!
//! 构造器复用父模块 `tests`；单独成文件以控制单文件规模。

use peri_acp::transport::AcpTransport;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::session::{PluginSelection, UserInput, UserInputQueueSnapshot};

use crate::kit::steer_state::SteerState;

use super::tests::RestoreProjection;
use super::{
    AcpTuiClient, Duration, STEERS, SteerCommand, SteerCommandKind, SteerFailure, SteerStage, atoms,
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
