use super::*;
use crate::kit::steer_state::SteerState;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
use peri_acp_types::{
    messages::MessageContent,
    session::{UserInput, UserInputQueueSnapshot},
};
use serde_json::json;

struct RestoreProjection {
    steers: SteerState,
    session: String,
    epoch: u64,
}

impl Drop for RestoreProjection {
    fn drop(&mut self) {
        STEERS.set(self.steers.clone());
        atoms::ACTIVE_SESSION_ID.set(self.session.clone());
        atoms::BRIDGE_RESET_COUNTER.set(self.epoch);
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_steer_uncertain_receipt_retries_identical_command_and_input() {
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
        },
        7,
        true,
    );
    let mut command = SteerCommand {
        session_id: "s".into(),
        epoch: 7,
        command_id: "stable-command".into(),
        generation: None,
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: uuid::Uuid::now_v7().to_string(),
            original_draft: "  中文\n@image /tmp/a.png\n".into(),
            content: MessageContent::text("  中文\n@image /tmp/a.png\n"),
        }),
    };
    projection.begin(command.clone());
    STEERS.set(projection);
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notifications, _) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s", false);
    client.spawn_pump(notifications);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request {
            id,
            method,
            params: original,
        } = server_transport.recv().await.unwrap()
        else {
            panic!("应收到首次 enqueue");
        };
        assert_eq!(
            method, "session/input/enqueue",
            "首次操作必须走专用入队协议"
        );
        server_transport
            .send_response(id, Err(AcpError::new(-32603, "lost receipt")))
            .await
            .unwrap();
        let IncomingMessage::Request {
            id,
            method,
            params: retry,
        } = server_transport.recv().await.unwrap()
        else {
            panic!("应收到同身份重试");
        };
        assert_eq!(
            method, "session/input/enqueue",
            "不确定结果不能切回 session/prompt"
        );
        assert_eq!(
            retry, original,
            "重试的全部字段必须相同，包括命令、输入和 generation"
        );
        server_transport
            .send_response(
                id,
                Ok(json!({
                    "snapshot":{"sessionId":"s","generation":"g","revision":2,"items":[{
                        "inputId":retry["inputId"], "originalDraft":retry["originalDraft"],
                        "content":retry["content"], "state":"queued"
                    }]}, "results":[{"inputId":retry["inputId"],"state":"queued"}]
                })),
            )
            .await
            .unwrap();
    });
    let error = execute(&client, &mut command, "/tmp").await.unwrap_err();
    assert_eq!(error.code, -32603, "首次回执结果不明确");
    STEERS.state().write().reject(&command, false);
    assert!(
        STEERS.state().read().pending_recovery_ids("s").is_empty(),
        "不明确输入不能变成可重复提交草稿"
    );
    atoms::BRIDGE_RESET_COUNTER.set(8);
    {
        let atom = STEERS.state();
        let mut projection = atom.write();
        projection.reset_session("s", 8);
        projection.accept_snapshot(
            UserInputQueueSnapshot {
                session_id: "s".into(),
                generation: "g".into(),
                revision: 1,
                active_request_id: None,
                items: vec![],
            },
            8,
            true,
        );
        let resumed = projection.resume_pending("s", 8);
        assert_eq!(resumed.len(), 1, "相同实例重载后应恢复原未决命令");
        assert_eq!(
            resumed[0].command_id, command.command_id,
            "重载重试不可更换命令身份"
        );
    }
    execute(&client, &mut command, "/tmp").await.unwrap();
    let rows = STEERS.state().read().rows("s");
    assert_eq!(rows.len(), 1, "确认重试后只保留一个权威队列项");
    assert_eq!(
        rows[0].state,
        crate::kit::steer_queue::SteerItemState::Queued,
        "确认后才开放动作"
    );
    server.await.unwrap();
    client.close();
}

async fn initial_session_failure_recovers_draft(snapshot_failure: bool) {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    atoms::ACTIVE_SESSION_ID.set(String::new());
    atoms::BRIDGE_RESET_COUNTER.set(3);
    STEERS.set(SteerState::default());
    let raw = "  尚未发送\n@image /tmp/a.png\n";
    let mut command = SteerCommand {
        session_id: String::new(),
        epoch: 3,
        command_id: "first-command".into(),
        generation: None,
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: uuid::Uuid::now_v7().to_string(),
            original_draft: raw.into(),
            content: MessageContent::text(raw),
        }),
    };
    STEERS.state().write().begin(command.clone());
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notifications, _) = AcpTuiClient::new_interactive(client_transport);
    client.spawn_pump(notifications);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, .. } = server_transport.recv().await.unwrap()
        else {
            panic!("应协商能力");
        };
        assert_eq!(method, "initialize", "首会话之前协商");
        server_transport
            .send_response(
                id,
                Ok(json!({"agentCapabilities":{"_meta":{"peri.userInputQueue":true}}})),
            )
            .await
            .unwrap();
        let IncomingMessage::Request { id, method, .. } = server_transport.recv().await.unwrap()
        else {
            panic!("应创建首会话");
        };
        assert_eq!(method, "session/new", "入队前准备真实会话");
        if snapshot_failure {
            server_transport
                .send_response(id, Ok(json!({"sessionId":"new"})))
                .await
                .unwrap();
            let IncomingMessage::Request { id, method, .. } =
                server_transport.recv().await.unwrap()
            else {
                panic!("应查询实例");
            };
            assert_eq!(method, "session/input/snapshot", "绑定实例前不能发送输入");
            server_transport
                .send_response(id, Err(AcpError::new(-32603, "snapshot failed")))
                .await
                .unwrap();
        } else {
            server_transport
                .send_response(id, Err(AcpError::new(-32603, "session unavailable")))
                .await
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), server_transport.recv())
                .await
                .is_err(),
            "准备失败时不能发出 enqueue 或 fallback prompt"
        );
    });
    client.register_ui_commands(&[]).await.unwrap();
    let error = execute(&client, &mut command, "/tmp").await.unwrap_err();
    assert!(
        atoms::BRIDGE_RESET_COUNTER.get() > 3,
        "真实交互 client 已推进会话边界"
    );
    assert!(
        reject_command(&mut command, &error),
        "入队之前的失败应确认为未受理"
    );
    let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let epoch = atoms::BRIDGE_RESET_COUNTER.get();
    assert!(
        !STEERS
            .state()
            .read()
            .pending_recovery_ids(&session_id)
            .is_empty(),
        "旧空会话 epoch 不得隐藏未发送原稿"
    );
    assert_eq!(
        STEERS
            .state()
            .write()
            .recover(&session_id, epoch, true)
            .unwrap()
            .original_draft,
        raw,
        "首会话准备失败仍须完整恢复多行及图片引用"
    );
    server.await.unwrap();
    client.close();
}

#[tokio::test]
#[serial_test::serial]
async fn test_steer_initial_new_session_failure_recovers_unsubmitted_draft() {
    initial_session_failure_recovers_draft(false).await;
}

#[tokio::test]
#[serial_test::serial]
async fn test_steer_initial_snapshot_failure_recovers_unsubmitted_draft() {
    initial_session_failure_recovers_draft(true).await;
}

#[tokio::test]
#[serial_test::serial]
async fn test_steer_queued_retry_never_replays_into_a_new_server_generation() {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    atoms::ACTIVE_SESSION_ID.set("s".into());
    atoms::BRIDGE_RESET_COUNTER.set(7);
    let snapshot = |generation: &str| UserInputQueueSnapshot {
        session_id: "s".into(),
        generation: generation.into(),
        revision: 1,
        active_request_id: None,
        items: vec![],
    };
    let mut projection = SteerState::default();
    projection.reset_session("s", 7);
    projection.accept_snapshot(snapshot("old-instance"), 7, true);
    let input_id = uuid::Uuid::now_v7().to_string();
    let mut command = SteerCommand {
        session_id: "s".into(),
        epoch: 7,
        command_id: "uncertain-command".into(),
        generation: None,
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: input_id.clone(),
            original_draft: "原回执尚未核实".into(),
            content: MessageContent::text("原回执尚未核实"),
        }),
    };
    projection.begin(command.clone());
    STEERS.set(projection);
    let (client_transport, server_transport) = mpsc_transport_pair();
    let server_transport = std::sync::Arc::new(server_transport);
    let (client, notifications, _) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s", false);
    client.spawn_pump(notifications);
    let first_server = server_transport.clone();
    let first_response = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, params } = first_server.recv().await.unwrap()
        else {
            panic!("应收到首次 enqueue");
        };
        assert_eq!(method, "session/input/enqueue", "首次请求使用专用准入");
        assert_eq!(params["generation"], "old-instance", "首次请求绑定原实例");
        first_server
            .send_response(id, Err(AcpError::new(-32603, "receipt unavailable")))
            .await
            .unwrap();
    });
    let error = execute(&client, &mut command, "/tmp").await.unwrap_err();
    assert!(
        !reject_command(&mut command, &error),
        "在途请求失败仍是未知结果"
    );
    first_response.await.unwrap();
    let mut retries = VecDeque::from([command]);

    atoms::BRIDGE_RESET_COUNTER.set(8);
    {
        let atom = STEERS.state();
        let mut projection = atom.write();
        projection.reset_session("s", 8);
        projection.accept_snapshot(snapshot("new-instance"), 8, true);
        assert!(
            projection.resume_pending("s", 8).is_empty(),
            "新实例不能主动恢复旧请求"
        );
    }
    let mut old_retry = retries.pop_front().unwrap();
    let (result, unexpected_method) =
        tokio::join!(execute(&client, &mut old_retry, "/tmp"), async {
            match tokio::time::timeout(Duration::from_millis(30), server_transport.recv()).await {
                Ok(Some(IncomingMessage::Request { id, method, .. })) => {
                    server_transport
                        .send_response(id, Err(AcpError::new(-32602, "stale generation")))
                        .await
                        .unwrap();
                    Some(method)
                }
                _ => None,
            }
        });
    assert!(result.is_ok(), "旧重试应保留未知结果，而不是转成明确未受理");
    assert!(
        unexpected_method.is_none(),
        "consumer中残留旧重试也不能发到新实例"
    );
    let projection = STEERS.state();
    let projection = projection.read();
    assert!(
        projection.pending_recovery_ids("s").is_empty(),
        "不得恢复成可以重复发送的草稿"
    );
    assert!(
        projection
            .pending_command("s", "uncertain-command")
            .is_some(),
        "保留原命令等待核对"
    );
    assert!(
        projection.rows("s").iter().any(|row| row.id == input_id),
        "未知输入原稿仍须可见"
    );
    drop(projection);
    client.close();
}
