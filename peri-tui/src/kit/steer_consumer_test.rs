use super::*;
use crate::kit::steer_state::SteerState;
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
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
    let SteerFailure { error, stage } = execute(&client, &mut command, "/tmp").await.unwrap_err();
    assert_eq!(error.code, -32603, "首次回执结果不明确");
    assert_eq!(stage, SteerStage::Admit, "会话已建立，失败属于入队阶段");
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
            let IncomingMessage::Request { id, method, params } =
                server_transport.recv().await.unwrap()
            else {
                panic!("初始化失败必须关闭已创建的会话");
            };
            assert_eq!(method, "session/close", "清理不是enqueue或fallback prompt");
            assert_eq!(params, json!({"sessionId":"new"}));
            server_transport
                .send_response(id, Ok(json!({})))
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
    let failure = execute(&client, &mut command, "/tmp").await.unwrap_err();
    assert!(!client.has_session(), "失败不能留下可发送输入的会话");
    assert!(atoms::ACTIVE_SESSION_ID.state().read().is_empty());
    assert!(client.current_execution_cwd().is_none());
    assert!(
        atoms::BRIDGE_RESET_COUNTER.get() > 3,
        "真实交互 client 已推进会话边界"
    );
    assert_eq!(
        failure.stage,
        SteerStage::Prepare,
        "会话建立（含其初始化）期间的失败都属于准备阶段"
    );
    let rejected = reject_command(&mut command, &failure.error);
    assert!(rejected, "入队之前的失败应确认为未受理");
    let notice = failure_notice(failure.stage, rejected, &failure.error);
    let reason = if snapshot_failure {
        "snapshot failed"
    } else {
        "session unavailable"
    };
    assert!(
        notice.contains(reason) && notice != crate::i18n::tr("steer-input-rejected"),
        "准备会话失败必须说明服务端给出的原因：{notice}"
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

#[test]
fn test_failure_notice_distinguishes_preparation_from_admission() {
    let reason = "session directory changed";
    let error = AcpError::new(-32010, reason);
    let prepared = failure_notice(SteerStage::Prepare, true, &error);
    assert!(
        prepared.contains(reason),
        "准备失败必须复述服务端给出的原因：{prepared}"
    );
    assert_eq!(
        failure_notice(SteerStage::Admit, true, &error),
        crate::i18n::tr("steer-input-rejected"),
        "入队被拒沿用原结论"
    );
    assert_eq!(
        failure_notice(SteerStage::Admit, false, &error),
        crate::i18n::tr("steer-input-uncertain"),
        "未知回执沿用原结论"
    );
}

/// 只读准入的会话上，宿主的 `-32010` 是确定结论，不是「回执不明」。
///
/// 判据只是客户端已经持有的准入事实（`SESSION_READ_ONLY`），不是新加的输入闸门：请求
/// 照发，结论仍由宿主的 `require_owner` 给出；这里只保证呈现口径——原稿按确定拒绝还给
/// composer，不把只读会话的提交挂成每 5s 重投的待定态。
#[test]
#[serial_test::serial]
fn test_read_only_session_submission_is_a_determined_rejection() {
    use peri_acp_types::workspace::ReadOnlyAdmission;

    struct RestoreReadOnly(Option<ReadOnlyAdmission>);
    impl Drop for RestoreReadOnly {
        fn drop(&mut self) {
            atoms::SESSION_READ_ONLY.set(self.0.take());
        }
    }

    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    let _read_only = RestoreReadOnly(atoms::SESSION_READ_ONLY.state().read().clone());
    atoms::ACTIVE_SESSION_ID.set("s".into());
    atoms::BRIDGE_RESET_COUNTER.set(7);
    let ownership_denied = AcpError::new(-32010, "session is owned by another execution host");
    let draft = "  中文草稿\n@image /tmp/a.png\n";
    let command = SteerCommand {
        session_id: "s".into(),
        epoch: 7,
        command_id: "stable-command".into(),
        // snapshot 已经成功、入队请求已发出：`-32010` 来自入队本身。
        generation: Some("g".into()),
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: "input-1".into(),
            original_draft: draft.into(),
            content: MessageContent::text(draft),
        }),
    };

    atoms::SESSION_READ_ONLY.set(Some(ReadOnlyAdmission::ExecutionBusy));
    let mut projection = SteerState::default();
    projection.reset_session("s", 7);
    projection.begin(command.clone());
    STEERS.set(projection);
    let mut read_only_command = command.clone();
    assert!(
        reject_command(&mut read_only_command, &ownership_denied),
        "只读会话的执行所有权拒绝是确定结论"
    );
    assert_eq!(
        STEERS
            .state()
            .write()
            .recover("s", 7, true)
            .map(|input| input.original_draft),
        Some(draft.to_string()),
        "确定拒绝必须把原稿还给 composer"
    );

    // 对照：没有只读准入事实时，同一错误仍是「回执不明」——占用可能只是瞬时的。
    atoms::SESSION_READ_ONLY.set(None);
    let mut projection = SteerState::default();
    projection.reset_session("s", 7);
    projection.begin(command.clone());
    STEERS.set(projection);
    let mut uncertain_command = command.clone();
    assert!(
        !reject_command(&mut uncertain_command, &ownership_denied),
        "瞬时的执行占用不得被改判为确定拒绝"
    );
    assert!(
        STEERS.state().read().pending_recovery_ids("s").is_empty(),
        "回执不明不能变成可重复提交草稿"
    );
}

#[test]
fn test_steer_session_unavailable_notice_is_translated_in_both_locales() {
    let error = "session unavailable".to_string();
    for lang in ["en", "zh-CN"] {
        let registry = crate::i18n::LcRegistry::new(Some(lang));
        let args = vec![("error".to_string(), error.clone().into())];
        let notice = registry.tr_args("steer-session-unavailable", &args);
        assert!(
            notice.contains(&error) && !notice.contains("steer-session-unavailable"),
            "{lang} 缺少 steer-session-unavailable 文案：{notice}"
        );
    }
}

#[test]
fn test_steer_read_only_notice_is_translated_in_both_locales() {
    for lang in ["en", "zh-CN"] {
        let registry = crate::i18n::LcRegistry::new(Some(lang));
        let notice = registry.tr("steer-session-read-only");
        assert!(
            !notice.is_empty() && !notice.contains("steer-session-read-only"),
            "{lang} 缺少 steer-session-read-only 文案：{notice}"
        );
    }
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
    let SteerFailure { error, .. } = execute(&client, &mut command, "/tmp").await.unwrap_err();
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

/// 应答能力协商；返回后 `ensure_session` 才会发起 `session/new`。
async fn answer_initialize(server: &MpscServerTransport) {
    let IncomingMessage::Request { id, method, .. } = server.recv().await.unwrap() else {
        panic!("应协商能力");
    };
    assert_eq!(method, "initialize", "首会话之前协商");
    server
        .send_response(
            id,
            Ok(json!({"agentCapabilities":{"_meta":{"peri.userInputQueue":true}}})),
        )
        .await
        .unwrap();
}

/// 接收 `session/new`；应答时机由调用方决定（慢应答或不应答）。
async fn expect_session_new(server: &MpscServerTransport) -> RequestId {
    let IncomingMessage::Request { id, method, .. } = server.recv().await.unwrap() else {
        panic!("应创建首会话");
    };
    assert_eq!(method, "session/new", "入队前准备真实会话");
    id
}

/// 以真实 consumer 驱动输入链路，返回发送端与关闭句柄。
fn start_consumer(
    client: &AcpTuiClient,
) -> (
    tokio::sync::mpsc::UnboundedSender<SteerCommand>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_steer_consumer(client.clone(), rx, "/tmp".into(), shutdown.clone());
    (tx, shutdown, handle)
}

/// 首会话之前提交的输入：session ID 尚未确定。
fn first_input_command(epoch: u64, command_id: &str, input_id: &str, draft: &str) -> SteerCommand {
    SteerCommand {
        session_id: String::new(),
        epoch,
        command_id: command_id.into(),
        generation: None,
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: input_id.into(),
            original_draft: draft.into(),
            content: MessageContent::text(draft),
        }),
    }
}

fn original_draft(command: &SteerCommand) -> String {
    match &command.kind {
        SteerCommandKind::Enqueue(input) => input.original_draft.clone(),
        other => panic!("期望入队命令：{other:?}"),
    }
}

fn notice_text() -> Option<String> {
    atoms::NOTIFICATION
        .state()
        .read()
        .as_ref()
        .map(|notice| notice.message.clone())
}

/// 通知不可 Clone；按原消息重新发布，避免测试清掉其它用例留下的状态。
fn restore_notice(previous: Option<String>) {
    atoms::NOTIFICATION.set(previous.map(|message| atoms::Notification {
        message,
        until: std::time::Instant::now() + FAILURE_NOTICE_DURATION,
    }));
}

/// 慢准备（超过受理回执预算）仍须被受理。
///
/// 准备阶段与会话已发出请求的回执不是同一预算：建会话要继续完成工作区发现、
/// 服务端准入并等待 operation gate。共用期限会把「还没来得及发送」判成输入被拒，
/// 用户既看不到原因也拿不到原稿以外的结论。
#[tokio::test(start_paused = true)]
#[serial_test::serial]
async fn test_slow_session_preparation_is_not_capped_by_receipt_deadline() {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    let previous_notice = notice_text();
    atoms::NOTIFICATION.set(None);
    atoms::ACTIVE_SESSION_ID.set(String::new());
    atoms::BRIDGE_RESET_COUNTER.set(31);
    STEERS.set(SteerState::default());
    let input_id = uuid::Uuid::now_v7().to_string();
    let command = first_input_command(31, "slow-prepare", &input_id, "慢准备的输入");
    STEERS.state().write().begin(command.clone());

    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notifications, _) = AcpTuiClient::new_interactive(client_transport);
    client.spawn_pump(notifications);
    let enqueued = input_id.clone();
    let server = tokio::spawn(async move {
        answer_initialize(&server_transport).await;
        let create = expect_session_new(&server_transport).await;
        // 准备在途时必须已经置位：请求都已发出，用户此刻只应看到「正在准备会话」。
        assert!(
            atoms::SESSION_PREPARING.get(),
            "会话建立请求发出后，准备阶段必须对用户可见"
        );
        // 慢 host：建会话超过受理回执预算，但仍在准备阶段期限内。
        tokio::time::sleep(RECEIPT_TIMEOUT + Duration::from_secs(5)).await;
        server_transport
            .send_response(create, Ok(json!({"sessionId":"slow"})))
            .await
            .unwrap();
        // 服务端快照必须自洽：核对刷新拿到空队列会抹掉已确认的待发项。
        let mut admitted: Option<serde_json::Value> = None;
        loop {
            let Ok(Some(message)) =
                tokio::time::timeout(Duration::from_secs(120), server_transport.recv()).await
            else {
                panic!("慢准备完成后仍未收到输入入队");
            };
            let IncomingMessage::Request { id, method, params } = message else {
                continue;
            };
            match method.as_str() {
                "session/input/snapshot" => {
                    let snapshot = admitted.clone().unwrap_or_else(
                        || json!({"sessionId":"slow","generation":"g","revision":1,"items":[]}),
                    );
                    server_transport
                        .send_response(id, Ok(snapshot))
                        .await
                        .unwrap();
                }
                "session/input/enqueue" => {
                    assert_eq!(params["inputId"], enqueued, "入队必须复用原输入身份");
                    // 入队已经发生在准备之后：可见状态必须随之收尾。
                    assert!(
                        !atoms::SESSION_PREPARING.get(),
                        "会话建立后不得继续显示准备状态"
                    );
                    let snapshot = json!({"sessionId":"slow","generation":"g","revision":2,
                        "items":[{"inputId":params["inputId"],
                            "originalDraft":params["originalDraft"],
                            "content":params["content"],"state":"queued"}]});
                    admitted = Some(snapshot.clone());
                    server_transport
                        .send_response(
                            id,
                            Ok(json!({
                                "snapshot": snapshot,
                                "results":[{"inputId":params["inputId"],"state":"queued"}]
                            })),
                        )
                        .await
                        .unwrap();
                    // 之后只允许核对快照的实例刷新；再次投递同一输入才是重复执行。
                    let duplicate =
                        tokio::time::timeout(RECEIPT_TIMEOUT + Duration::from_secs(1), async {
                            while let Some(message) = server_transport.recv().await {
                                if let IncomingMessage::Request { id, method, .. } = message {
                                    if method == "session/input/enqueue" {
                                        return Some(method);
                                    }
                                    server_transport
                                        .send_response(id, Ok(admitted.clone().unwrap()))
                                        .await
                                        .unwrap();
                                }
                            }
                            None
                        })
                        .await;
                    if let Ok(Some(method)) = duplicate {
                        panic!("同一输入不得重复投递，却收到 {method}");
                    }
                    return;
                }
                other => panic!("慢准备期间出现意外请求：{other}"),
            }
        }
    });
    client.register_ui_commands(&[]).await.unwrap();
    let (tx, shutdown, handle) = start_consumer(&client);
    tx.send(command).unwrap();
    server.await.unwrap();

    assert_eq!(
        notice_text(),
        None,
        "慢准备成功不得留下失败提示：{:?}",
        notice_text()
    );
    let rows = STEERS.state().read().rows("slow");
    assert_eq!(rows.len(), 1, "受理后应恰好一条队列项");
    assert_eq!(rows[0].id, input_id, "入队身份必须原样保留");
    assert_eq!(
        rows[0].state,
        crate::kit::steer_queue::SteerItemState::Queued,
        "回执确认后才开放动作"
    );
    assert!(
        STEERS
            .state()
            .read()
            .pending_command("slow", "slow-prepare")
            .is_none(),
        "落定后不得留在未决命令里"
    );
    shutdown.cancel();
    handle.await.unwrap();
    restore_notice(previous_notice);
    client.close();
}

/// 准备超时按准备阶段报告并恢复原稿：输入从未发出，不能表述为「输入未被接收」。
#[tokio::test(start_paused = true)]
#[serial_test::serial]
async fn test_session_preparation_timeout_recovers_draft_without_admission() {
    let _restore = RestoreProjection {
        steers: STEERS.state().read().clone(),
        session: atoms::ACTIVE_SESSION_ID.state().read().clone(),
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
    };
    let previous_notice = notice_text();
    atoms::NOTIFICATION.set(None);
    atoms::ACTIVE_SESSION_ID.set(String::new());
    atoms::BRIDGE_RESET_COUNTER.set(32);
    STEERS.set(SteerState::default());
    let input_id = uuid::Uuid::now_v7().to_string();
    let raw = "  准备超时的输入\n@image /tmp/a.png\n";
    let command = first_input_command(32, "prepare-timeout", &input_id, raw);
    STEERS.state().write().begin(command.clone());

    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notifications, _) = AcpTuiClient::new_interactive(client_transport);
    client.spawn_pump(notifications);
    let server = tokio::spawn(async move {
        answer_initialize(&server_transport).await;
        let _create = expect_session_new(&server_transport).await;
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
    client.register_ui_commands(&[]).await.unwrap();
    let (tx, shutdown, handle) = start_consumer(&client);
    tx.send(command).unwrap();
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
    let notice = notice_text().expect("准备超时必须给出提示");
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
    handle.await.unwrap();
    server.await.unwrap();
    restore_notice(previous_notice);
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
    let previous_notice = notice_text();
    atoms::NOTIFICATION.set(None);
    atoms::ACTIVE_SESSION_ID.set(String::new());
    atoms::BRIDGE_RESET_COUNTER.set(33);
    STEERS.set(SteerState::default());
    let input_id = uuid::Uuid::now_v7().to_string();
    let command = first_input_command(33, "cancelled-prepare", &input_id, "取消前的输入");
    STEERS.state().write().begin(command.clone());

    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notifications, _) = AcpTuiClient::new_interactive(client_transport);
    client.spawn_pump(notifications);
    let server = tokio::spawn(async move {
        answer_initialize(&server_transport).await;
        let _create = expect_session_new(&server_transport).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(30), server_transport.recv())
                .await
                .is_err(),
            "取消后不得发送入队请求"
        );
    });
    client.register_ui_commands(&[]).await.unwrap();
    let (tx, shutdown, handle) = start_consumer(&client);
    tx.send(command).unwrap();
    // 超过受理回执预算：准备仍在进行，输入既未被拒也没有提示。
    tokio::time::sleep(RECEIPT_TIMEOUT + Duration::from_secs(1)).await;
    assert_eq!(
        notice_text(),
        None,
        "准备期间不得提前宣告失败：{:?}",
        notice_text()
    );
    assert!(
        atoms::SESSION_PREPARING.get(),
        "准备尚未返回时必须保持可见状态"
    );
    shutdown.cancel();
    handle.await.unwrap();
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
    assert_eq!(original_draft(&pending), "取消前的输入", "取消不得改写原稿");
    server.await.unwrap();
    restore_notice(previous_notice);
    client.close();
}
