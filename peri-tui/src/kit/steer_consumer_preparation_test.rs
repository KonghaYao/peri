//! 会话准备阶段的超时与取消回归测试。
//!
//! 构造器复用父模块 `tests`；单独成文件以控制单文件规模。

use crate::kit::steer_state::SteerState;
use peri_acp::transport::AcpTransport;

use super::tests::RECEIPT_TIMEOUT;
use super::tests::RestoreProjection;
use super::{AcpTuiClient, Duration, PREPARE_TIMEOUT, STEERS, atoms};

/// 准备超时按准备阶段报告并恢复原稿：输入从未发出，不能表述为「输入未被接收」。
#[tokio::test(start_paused = true)]
#[serial_test::serial]
async fn test_session_preparation_timeout_recovers_draft_without_admission() {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    let previous_notice = super::tests::notice_text();
    atoms::NOTIFICATION.set(None);
    atoms::ACTIVE_SESSION_ID.set(String::new());
    atoms::BRIDGE_RESET_COUNTER.set(32);
    STEERS.set(SteerState::default());
    let input_id = uuid::Uuid::now_v7().to_string();
    let raw = "  准备超时的输入\n@image /tmp/a.png\n";
    let command = super::tests::first_input_command(32, "prepare-timeout", &input_id, raw);
    STEERS.state().write().begin(command.clone());

    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notifications, _) = AcpTuiClient::new_interactive(client_transport);
    client.spawn_pump(notifications);
    let server = tokio::spawn(async move {
        super::tests::answer_initialize(&server_transport).await;
        let _create = super::tests::expect_session_new(&server_transport).await;
        // 服务端始终不回答建会话：只有准备阶段自己的期限可以作出结论。
        assert!(
            tokio::time::timeout(
                PREPARE_TIMEOUT + Duration::from_secs(5),
                server_transport.recv()
            )
            .await
            .is_err(),
            "准备超时不得发送任何入队请求"
        );
    });
    client.register_ui_commands(&[]).await.expect("session preparation timeout recovers draft without admission: fixture task and request must complete successfully");
    let (tx, shutdown, handle) = super::tests::start_consumer(&client);
    tx.send(command).expect("session preparation timeout recovers draft without admission: fixture channel must deliver its expected message");
    let recovered = tokio::time::timeout(PREPARE_TIMEOUT + Duration::from_secs(5), async {
        loop {
            if !STEERS.state().read().pending_recovery_ids("").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(recovered.is_ok(), "准备超时后必须恢复未发送原稿");
    assert!(
        !atoms::SESSION_PREPARING.get(),
        "准备超时后不得把状态栏留在「正在准备会话」"
    );
    let notice = super::tests::notice_text().expect("准备超时必须给出提示");
    assert!(
        notice.contains("timed out") && notice != crate::i18n::tr("steer-input-rejected"),
        "准备超时不得表述为输入被拒：{notice}"
    );
    assert_eq!(
        STEERS
            .state()
            .write()
            .recover("", atoms::BRIDGE_RESET_COUNTER.get(), true)
            .expect("恢复原稿")
            .original_draft,
        raw,
        "准备超时仍须完整恢复多行及图片引用"
    );
    shutdown.cancel();
    handle.await.expect("session preparation timeout recovers draft without admission: fixture task and request must complete successfully");
    server.await.expect("session preparation timeout recovers draft without admission: fixture task and request must complete successfully");
    super::tests::restore_notice(previous_notice);
    client.close();
}

/// 准备期间取消：未受理的输入保持未决身份，不得变成已发送或重复投递。
#[tokio::test(start_paused = true)]
#[serial_test::serial]
async fn test_shutdown_during_preparation_keeps_unadmitted_input_identity() {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    let previous_notice = super::tests::notice_text();
    atoms::NOTIFICATION.set(None);
    atoms::ACTIVE_SESSION_ID.set(String::new());
    atoms::BRIDGE_RESET_COUNTER.set(33);
    STEERS.set(SteerState::default());
    let input_id = uuid::Uuid::now_v7().to_string();
    let command =
        super::tests::first_input_command(33, "cancelled-prepare", &input_id, "取消前的输入");
    STEERS.state().write().begin(command.clone());

    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notifications, _) = AcpTuiClient::new_interactive(client_transport);
    client.spawn_pump(notifications);
    let server = tokio::spawn(async move {
        super::tests::answer_initialize(&server_transport).await;
        let _create = super::tests::expect_session_new(&server_transport).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(30), server_transport.recv())
                .await
                .is_err(),
            "取消后不得发送入队请求"
        );
    });
    client.register_ui_commands(&[]).await.expect("shutdown during preparation keeps unadmitted input identity: fixture task and request must complete successfully");
    let (tx, shutdown, handle) = super::tests::start_consumer(&client);
    tx.send(command).expect("shutdown during preparation keeps unadmitted input identity: fixture channel must deliver its expected message");
    // 超过受理回执预算：准备仍在进行，输入既未被拒也没有提示。
    tokio::time::sleep(RECEIPT_TIMEOUT + Duration::from_secs(1)).await;
    assert_eq!(
        super::tests::notice_text(),
        None,
        "准备期间不得提前宣告失败：{:?}",
        super::tests::notice_text()
    );
    assert!(
        atoms::SESSION_PREPARING.get(),
        "准备尚未返回时必须保持可见状态"
    );
    shutdown.cancel();
    handle.await.expect("shutdown during preparation keeps unadmitted input identity: fixture task and request must complete successfully");
    assert!(
        !atoms::SESSION_PREPARING.get(),
        "应用关闭丢弃准备中的 future 后，可见状态必须被清除（Drop 而非逐分支清理）"
    );
    let pending = STEERS
        .state()
        .read()
        .pending_command("", "cancelled-prepare")
        .cloned()
        .expect("取消不得丢弃未受理输入的身份");
    assert_eq!(
        super::tests::original_draft(&pending),
        "取消前的输入",
        "取消不得改写原稿"
    );
    server.await.expect("shutdown during preparation keeps unadmitted input identity: fixture task and request must complete successfully");
    super::tests::restore_notice(previous_notice);
    client.close();
}
