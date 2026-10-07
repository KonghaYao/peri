//! 用户队列的控制请求、执行准入与 MPSC/stdio 契约。

use super::*;

async fn make_user_input_session(
    tmp: &tempfile::TempDir,
) -> (AcpServerConfig, HashMap<String, SessionState>, String) {
    let config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "unused-test-key",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config, provider, tmp).await;
    let mut sessions = HashMap::new();
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager
        .ensure_session(&sid, tmp.path().to_str().unwrap());
    cfg.session_manager.ensure_session_caps(&sid);
    (cfg, sessions, sid)
}

fn make_user_input_request(sid: &str, generation: &str, input_id: &str, text: &str) -> Value {
    json!({
        "sessionId": sid,
        "generation": generation,
        "commandId": format!("enqueue-{input_id}"),
        "inputId": input_id,
        "content": text,
        "originalDraft": text,
    })
}

#[tokio::test]
async fn inbox_notifications_outlive_mailbox_invalidation_and_stop_on_session_close() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, _, sid) = make_user_input_session(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    crate::host::user_input::ensure_mailbox(&sid, &cfg, &transport)
        .await
        .unwrap();
    let cancellation = cfg
        .session_manager
        .get_session(&sid)
        .unwrap()
        .inbox_work_notifications
        .as_ref()
        .unwrap()
        .1
        .clone();
    cfg.session_manager.invalidate_user_input_mailbox(&sid);
    assert!(!cancellation.is_cancelled());
    crate::host::user_input::ensure_mailbox(&sid, &cfg, &transport)
        .await
        .unwrap();
    assert!(!cancellation.is_cancelled());
    cfg.session_manager.pre_close_session(&sid);
    assert!(cancellation.is_cancelled());
}

#[tokio::test]
async fn durable_scan_notifies_required_work_without_a_queue_wake_hint() {
    use peri_acp_types::session_resources::work::*;
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, _, sid) = make_user_input_session(&tmp).await;
    let captured = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = captured.clone();
    crate::host::user_input::ensure_mailbox(&sid, &cfg, &transport)
        .await
        .unwrap();
    tokio::task::yield_now().await;
    let content = WorkPayload::from_payload(&peri_acp_types::store::PersistedPayload::Message(
        peri_acp_types::messages::BaseMessage::human("late durable work"),
    ))
    .unwrap();
    let receipt = cfg
        .session_resources
        .apply_work_mutation(&WorkCommand {
            session_id: sid.clone(),
            recipient_lifecycle: 1,
            mutation_id: "lost-hint-publication".into(),
            action: WorkAction::PublishDelivery {
                delivery: PublishDelivery {
                    delivery_id: "lost-hint-delivery".into(),
                    event: WorkEvent {
                        producer_namespace: "inbox-notification-test".into(),
                        event_id: "lost-hint-event".into(),
                        event_kind: "lateInput".into(),
                        causation_id: None,
                        content,
                    },
                    purpose: DeliveryPurpose::UserInput,
                    policy: peri_acp_types::session::MessagePolicy::ensure_processing(),
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if captured.notifications().iter().any(|(method, params)| {
                method == "session/work/available" && params["sessionId"] == sid
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("bounded scans must rediscover durable work without an MQ hint");
    let snapshot = cfg
        .session_resources
        .load_session_work(&WorkQuery {
            session_id: sid.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert!(snapshot.control.attempt.is_none());
    assert!(snapshot.state.admissions.is_empty());
    assert_eq!(
        snapshot.state.obligations["lost-hint-delivery"].status,
        ObligationStatus::Pending
    );
    cfg.session_manager.pre_close_session(&sid);
}

#[tokio::test]
async fn observer_scan_suppresses_history_but_not_new_or_explicit_publication() {
    use peri_acp_types::session_resources::work::*;
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, _, sid) = make_user_input_session(&tmp).await;
    let captured = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = captured.clone();
    let inbox = cfg
        .session_manager
        .get_session(&sid)
        .unwrap()
        .v2_message_queue
        .clone();
    let query = WorkQuery {
        session_id: sid.clone(),
        limit: 1,
    };
    let initial = cfg
        .session_resources
        .load_session_work(&query)
        .await
        .unwrap();
    let lifecycle = initial.control.lifecycle;
    let mut floor = initial.state.next_admission_sequence;
    for delivery_id in ["historical", "fresh"] {
        let content = WorkPayload::from_payload(&peri_acp_types::store::PersistedPayload::Message(
            peri_acp_types::messages::BaseMessage::human(delivery_id),
        ))
        .unwrap();
        let receipt = cfg
            .session_resources
            .apply_work_mutation(&WorkCommand {
                session_id: sid.clone(),
                recipient_lifecycle: lifecycle,
                mutation_id: delivery_id.into(),
                action: WorkAction::PublishDelivery {
                    delivery: PublishDelivery {
                        delivery_id: delivery_id.into(),
                        event: WorkEvent {
                            producer_namespace: "observer-history-test".into(),
                            event_id: delivery_id.into(),
                            event_kind: "input".into(),
                            causation_id: None,
                            content,
                        },
                        purpose: DeliveryPurpose::UserInput,
                        policy: peri_acp_types::session::MessagePolicy::ensure_processing(),
                    },
                },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
        if delivery_id == "historical" {
            floor = cfg
                .session_resources
                .load_session_work(&query)
                .await
                .unwrap()
                .state
                .next_admission_sequence;
        }
        for _ in 0..2 {
            crate::host::continuation::publish_inbox_work(
                cfg.session_resources.clone(),
                &transport,
                &sid,
                lifecycle,
                &inbox,
                Some(floor),
            )
            .await
            .unwrap();
        }
        let availability_count = captured
            .notifications()
            .iter()
            .filter(|(method, _)| method == "session/work/available")
            .count();
        assert_eq!(
            availability_count,
            if delivery_id == "historical" { 0 } else { 2 }
        );
    }
    crate::host::continuation::publish_inbox_work(
        cfg.session_resources.clone(),
        &transport,
        &sid,
        lifecycle,
        &inbox,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        captured
            .notifications()
            .iter()
            .filter(|(method, _)| method == "session/work/available")
            .count(),
        3
    );
    let snapshot = cfg
        .session_resources
        .load_session_work(&query)
        .await
        .unwrap();
    assert!(snapshot.control.attempt.is_none());
    assert!(snapshot.state.admissions.is_empty());
    assert_eq!(
        snapshot.state.obligations["historical"].status,
        ObligationStatus::Pending
    );
}

#[tokio::test]
async fn test_user_input_methods_require_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp).await;
    cfg.session_manager
        .caps_registry()
        .insert(sid.clone(), PeriCaps::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    for method in [
        "session/input/enqueue",
        "session/input/dispatch",
        "session/input/takeback",
        "session/input/snapshot",
    ] {
        let error = handle_request(
            method,
            &json!({"sessionId": sid}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32601, "未协商能力时所有队列 RPC 都应拒绝");
    }
    assert!(
        cfg.session_manager.user_input_mailbox_for(&sid).is_none(),
        "拒绝前不能建立队列 owner"
    );
}

#[tokio::test]
async fn test_user_input_takeback_and_dispatch_share_server_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let initial = handle_request(
        "session/input/snapshot",
        &json!({"sessionId": sid}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let generation = initial["generation"].as_str().unwrap();
    let mailbox = cfg.session_manager.user_input_mailbox_for(&sid).unwrap();
    mailbox
        .attach_external_attempt(tokio_util::sync::CancellationToken::new(), false)
        .unwrap();
    let first_id = "00000000-0000-0000-0000-000000000001";
    let second_id = "00000000-0000-0000-0000-000000000002";
    for (id, text) in [(first_id, "第一行\n第二行"), (second_id, "稍后处理")] {
        handle_request(
            "session/input/enqueue",
            &make_user_input_request(&sid, generation, id, text),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
    }
    let sent = handle_request(
        "session/input/dispatch",
        &json!({
            "sessionId": sid,
            "generation": generation,
            "commandId": "send-second",
            "inputIds": [second_id],
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let taken = handle_request(
        "session/input/takeback",
        &json!({
            "sessionId": sid,
            "generation": generation,
            "commandId": "take-first",
            "inputId": first_id,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(sent["results"][0]["state"], "dispatching", "只发布所选条目");
    assert_eq!(
        taken["takenBack"]["originalDraft"], "第一行\n第二行",
        "取回必须保留完整多行草稿"
    );
    assert_eq!(
        taken["snapshot"]["items"].as_array().unwrap().len(),
        1,
        "剩余队列只有已发布的第二条"
    );
    assert!(
        cfg.session_manager.v2_queue_for(&sid).unwrap().len() == 1,
        "只有可靠发布成功的第二条可作为 MQ 提示"
    );
}

#[tokio::test]
async fn test_user_input_generation_invalidation_rejects_old_command() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let old = handle_request(
        "session/input/snapshot",
        &json!({"sessionId": sid}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let stale_generation = format!("{}:stale", old["generation"].as_str().unwrap());
    let request = make_user_input_request(
        &sid,
        &stale_generation,
        "00000000-0000-0000-0000-000000000001",
        "旧请求",
    );
    let error = handle_request(
        "session/input/enqueue",
        &request,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    let data = error.data.unwrap();
    assert_eq!(error.code, -32602, "旧 generation 必须明确拒绝");
    assert_eq!(data["rejected"], true, "拒绝标记让客户端保留草稿");
    assert_ne!(
        data["snapshot"]["generation"], stale_generation,
        "生命周期身份不接受陈旧命令"
    );
    assert!(
        data["snapshot"]["items"].as_array().unwrap().is_empty(),
        "旧内容不能进入新实例"
    );
}

#[tokio::test]
async fn test_closing_user_input_session_can_read_but_cannot_mutate() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp).await;
    sessions.get_mut(&sid).unwrap().closing = true;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let snapshot = handle_request(
        "session/input/snapshot",
        &json!({"sessionId":sid}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let error = handle_request(
        "session/input/enqueue",
        &make_user_input_request(
            &sid,
            snapshot["generation"].as_str().unwrap(),
            "00000000-0000-0000-0000-000000000001",
            "不应提交",
        ),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32010);
    assert!(
        error.message.contains("closing"),
        "错误必须明确指出关闭状态"
    );
    assert!(
        cfg.session_manager
            .user_input_mailbox_for(&sid)
            .unwrap()
            .snapshot()
            .items
            .is_empty(),
        "拒绝不能修改队列"
    );
}

#[tokio::test]
async fn test_user_input_pause_preserves_durable_work_without_peri_ticket() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let snapshot = handle_request(
        "session/input/snapshot",
        &json!({"sessionId":sid}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    handle_request(
        "session/input/enqueue",
        &make_user_input_request(
            &sid,
            snapshot["generation"].as_str().unwrap(),
            "00000000-0000-0000-0000-000000000001",
            "尚未开始",
        ),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let mailbox = cfg.session_manager.user_input_mailbox_for(&sid).unwrap();
    assert!(mailbox.reserve_run().is_none());
    let control = cfg
        .session_resources
        .load_session_control(&sid)
        .await
        .unwrap();
    handle_request(
        "session/control",
        &json!({
            "sessionId":sid,"commandId":"pause-queued",
            "expectedLifecycle":control.lifecycle,"expectedRevision":control.revision,
            "expectedControlGeneration":control.control_generation,"action":{"kind":"pause"}
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(
        mailbox.snapshot().items[0].state,
        peri_acp_types::session::UserInputState::Dispatching,
        "Pause 保留持久义务，不能伪造已撤回"
    );
    assert!(
        mailbox.reserve_run().is_none(),
        "用户明确继续前不能自动复活"
    );
}

/// [回归测试] 长 prompt 持有执行锁时，真实 transport 上的队列操作仍须回复。
#[tokio::test]
async fn test_user_input_wire_control_responds_while_prompt_lock_is_held() {
    use crate::transport::AcpTransport;
    use tokio_util::sync::CancellationToken;
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut states, sid) = make_user_input_session(&tmp).await;
    let cfg = Arc::new(cfg);
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let client = Arc::new(client);
    let server: Arc<dyn AcpTransport> = Arc::new(server);
    let mailbox = crate::host::user_input::ensure_mailbox(&sid, &cfg, &server)
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    states.get_mut(&sid).unwrap().cancel_token = Some(cancel.clone());
    mailbox.attach_external_attempt(cancel, false).unwrap();
    let generation = mailbox.snapshot().generation;
    let states = Arc::new(tokio::sync::Mutex::new(states));
    let prompt_lock = Arc::new(tokio::sync::Mutex::new(()));
    let _running_prompt = Arc::clone(&prompt_lock).lock_owned().await;
    let locks = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        sid.clone(),
        prompt_lock,
    )])));
    let task = tokio::spawn(async move {
        let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
        let cont_tx = Arc::new(cont_tx);
        let connection = Arc::new(tokio::sync::Mutex::new(
            crate::host::connection::ConnectionContext::new(false),
        ));
        let cancellation = CancellationToken::new();
        crate::host::server_loop::ServerLoop {
            transport: &server,
            cfg: &cfg,
            sessions: &states,
            prompt_locks: &locks,
            cont_tx: &cont_tx,
            connection: &connection,
            connection_cancellation: &cancellation,
        }
        .run()
        .await;
    });
    let id = "00000000-0000-0000-0000-000000000001";
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.send_request(
            "session/input/enqueue",
            make_user_input_request(&sid, &generation, id, "运行中排队"),
        ),
    )
    .await
    .expect("队列请求不能等待 prompt 锁")
    .unwrap();
    assert_eq!(
        reply["results"][0]["state"], "dispatching",
        "运行中发送必须先可靠发布，执行仍由 SDK 串行准入"
    );
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.send_request(
            "session/input/takeback",
            json!({
                "sessionId": sid,
                "generation": generation,
                "commandId": "take-running",
                "inputId": id,
            }),
        ),
    )
    .await
    .expect("取回同样不能等待执行锁")
    .unwrap();
    assert_eq!(
        reply["takenBack"]["originalDraft"], "运行中排队",
        "真实 wire 应返回原文"
    );
    client.close();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("transport 关闭后 server loop 必须退出")
        .unwrap();
}

#[tokio::test]
async fn test_user_input_work_notification_does_not_launch_a_loop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp).await;
    let transport = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();
    let initial = handle_request(
        "session/input/snapshot",
        &json!({"sessionId":sid}),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let receipt = handle_request(
        "session/input/enqueue",
        &make_user_input_request(
            &sid,
            initial["generation"].as_str().unwrap(),
            "00000000-0000-0000-0000-000000000001",
            "等待 SDK 准入",
        ),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(receipt["workReceipts"][0]["decision"]["kind"], "accepted");
    assert_eq!(
        receipt["publicationGenerations"].as_object().unwrap().len(),
        1
    );
    let mailbox = cfg.session_manager.user_input_mailbox_for(&sid).unwrap();
    let cfg = Arc::new(cfg);
    let sessions = Arc::new(tokio::sync::Mutex::new(sessions));
    let locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    crate::host::user_input::schedule_mailbox(
        &sid,
        &sessions,
        &locks,
        &cfg,
        &transport_dyn,
        &Arc::new(sender),
    );
    let notice = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some((_, params)) = transport
                .notifications()
                .into_iter()
                .find(|(method, _)| method == "session/work/available")
            {
                break params;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(notice["sessionId"], sid);
    assert_eq!(notice["executionProtocol"], 1);
    assert!(mailbox.reserve_run().is_none());
    assert!(mailbox.snapshot().active_request_id.is_none());
}

#[tokio::test]
async fn test_user_input_stdio_uses_same_short_control_requests() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_util::sync::CancellationToken;
    let tmp = tempfile::TempDir::new().unwrap();
    let (mut cfg, states, sid) = make_user_input_session(&tmp).await;
    cfg.stdio_command_filter = true;
    let cfg = Arc::new(cfg);
    let (mut input, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output) = tokio::io::duplex(64 * 1024);
    let server: Arc<dyn crate::transport::AcpTransport> =
        Arc::new(crate::transport::stdio::StdioTransport::from_reader_writer(
            transport_read,
            transport_write,
        ));
    let mailbox = crate::host::user_input::ensure_mailbox(&sid, &cfg, &server)
        .await
        .unwrap();
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let generation = mailbox.snapshot().generation;
    let task = tokio::spawn(async move {
        let states = Arc::new(tokio::sync::Mutex::new(states));
        let locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
        let cont_tx = Arc::new(cont_tx);
        let connection = Arc::new(tokio::sync::Mutex::new(
            crate::host::connection::ConnectionContext::new(false),
        ));
        let cancellation = CancellationToken::new();
        crate::host::server_loop::ServerLoop {
            transport: &server,
            cfg: &cfg,
            sessions: &states,
            prompt_locks: &locks,
            cont_tx: &cont_tx,
            connection: &connection,
            connection_cancellation: &cancellation,
        }
        .run()
        .await;
    });
    let request = json!({
        "jsonrpc": "2.0",
        "id": 101,
        "method": "session/input/enqueue",
        "params": make_user_input_request(
            &sid,
            &generation,
            "00000000-0000-0000-0000-000000000001",
            "stdio 原文\n第二行",
        ),
    });
    input
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    input.flush().await.unwrap();
    let mut lines = BufReader::new(output).lines();
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("stdout 不应提前结束");
            let response: Value = serde_json::from_str(&line).unwrap();
            if response.get("id") == Some(&json!(101)) {
                break response;
            }
        }
    })
    .await
    .expect("stdio 控制请求必须及时回复");
    assert_eq!(
        response["result"]["results"][0]["state"], "dispatching",
        "stdio 与 MPSC 保持相同入队行为"
    );
    assert_eq!(
        response["result"]["snapshot"]["items"][0]["originalDraft"], "stdio 原文\n第二行",
        "真实 JSON 行协议保留全文与换行"
    );
    drop(input);
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("stdin EOF 应使宿主退出")
        .unwrap();
}

#[tokio::test]
async fn test_user_input_cancel_rejects_stale_ticket_without_cancelling_current_token() {
    use peri_agent::session::user_input_mailbox::UserInputAttemptOutcome;
    use tokio_util::sync::CancellationToken;
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let initial = handle_request(
        "session/input/snapshot",
        &json!({"sessionId":sid}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let generation = initial["generation"].as_str().unwrap();
    let input_id = "00000000-0000-0000-0000-000000000001";
    handle_request(
        "session/input/enqueue",
        &make_user_input_request(&sid, generation, input_id, "继续这条"),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let mailbox = cfg.session_manager.user_input_mailbox_for(&sid).unwrap();
    let old = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    mailbox.stop();
    mailbox.finish_attempt(&old, UserInputAttemptOutcome::Interrupted);
    handle_request(
        "session/input/dispatch",
        &json!({
            "sessionId": sid,
            "generation": generation,
            "commandId": "resume",
            "inputIds": [input_id],
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let current_cancel = CancellationToken::new();
    mailbox
        .attach_external_attempt(current_cancel.clone(), false)
        .unwrap();
    sessions.get_mut(&sid).unwrap().cancel_token = Some(current_cancel.clone());
    let old_target = peri_acp_types::session_resources::ControlAttempt {
        turn_id: peri_acp_types::session::TurnId::new(),
        attempt_id: peri_acp_types::identity::AttemptId::new(),
    };
    let new_target = peri_acp_types::session_resources::ControlAttempt {
        turn_id: peri_acp_types::session::TurnId::new(),
        attempt_id: peri_acp_types::identity::AttemptId::new(),
    };
    let control = cfg
        .session_resources
        .load_session_control(&sid)
        .await
        .unwrap();
    let old_stop = json!({"sessionId":sid,"commandId":"old-stop", "expectedLifecycle":control.lifecycle,
        "expectedRevision":control.revision,"expectedControlGeneration":control.control_generation,
        "action":{"kind":"stop","target":old_target}});
    cfg.session_resources
        .apply_session_control(&peri_acp_types::session_resources::ControlCommand {
            session_id: sid.clone(),
            command_id: "current-execution".into(),
            expected_lifecycle: control.lifecycle,
            expected_revision: control.revision,
            expected_control_generation: control.control_generation,
            action: peri_acp_types::session_resources::ControlAction::ObserveAttempt {
                target: Some(new_target.clone()),
            },
        })
        .await
        .unwrap();
    let receipt = handle_request(
        "session/control",
        &old_stop,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(receipt["decision"]["kind"], "rejected");
    assert!(
        !current_cancel.is_cancelled(),
        "旧 ticket 的迟到 Stop 不得取消新执行"
    );
    let control = cfg
        .session_resources
        .load_session_control(&sid)
        .await
        .unwrap();
    handle_request("session/control", &json!({"sessionId":sid,"commandId":"current-stop",
        "expectedLifecycle":control.lifecycle,"expectedRevision":control.revision,
        "expectedControlGeneration":control.control_generation,"action":{"kind":"stop","target":new_target}
    }), &cfg, &mut sessions, &transport).await.unwrap();
    assert!(
        current_cancel.is_cancelled(),
        "当前 ticket 的 Stop 必须传到真实执行 token"
    );
}

#[tokio::test]
async fn inbox_notification_uses_narrow_facts_with_undecodable_history() {
    use peri_acp_types::session_resources::work::*;
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, _, sid) = make_user_input_session(&tmp).await;
    let captured = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = captured.clone();
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        tmp.path().join("threads.db").display()
    ))
    .await
    .unwrap();
    let mut state = serde_json::to_value(WorkState::default()).unwrap();
    state["revision"] = json!(42);
    state["deliveries"]["required"] = json!({
        "recipientLifecycle": 1, "admissionSequence": 3, "batchId": null, "disposition": null,
        "publication": {"policy": {"requirement": "required"}}
    });
    state["obligations"]["required"] = json!({"status": "pending"});
    state["works"]["historical"] = json!({
        "workId": "historical", "batchId": "historical", "stage": "settled",
        "reasonRequest": {"serializedRequest": "history body".repeat(100_000)},
        "revision": "not a number"
    });
    assert!(serde_json::from_value::<WorkState>(state.clone()).is_err());
    sqlx::query("INSERT INTO session_work_state(session_id,state_json) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET state_json=excluded.state_json")
        .bind(&sid).bind(state.to_string()).execute(&pool).await.unwrap();
    let inbox = peri_acp_types::session::MessageQueue::new();
    for (lifecycle, floor, count) in [
        (1, Some(4), 0),
        (2, Some(3), 0),
        (1, Some(3), 1),
        (1, None, 2),
    ] {
        crate::host::continuation::publish_inbox_work(
            cfg.session_resources.clone(),
            &transport,
            &sid,
            lifecycle,
            &inbox,
            floor,
        )
        .await
        .unwrap();
        let notifications = captured.notifications();
        let available: Vec<_> = notifications
            .iter()
            .filter(|(method, _)| method == "session/work/available")
            .collect();
        assert_eq!(available.len(), count);
        if let Some((_, params)) = available.last() {
            assert_eq!(
                params,
                &json!({
                    "sessionId": sid, "revision": 42, "lifecycle": 1,
                    "controlGeneration": 0, "executionProtocol": 1,
                })
            );
        }
    }
    let cfg = Arc::new(cfg);
    let sessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    crate::host::user_input::schedule_mailbox(
        &sid,
        &sessions,
        &locks,
        &cfg,
        &transport,
        &Arc::new(sender),
    );
    let notice = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some((_, params)) = captured
                .notifications()
                .into_iter()
                .filter(|(method, _)| method == "session/work/available")
                .nth(2)
            {
                break params;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        notice,
        json!({
            "sessionId": sid, "revision": 42, "lifecycle": 1,
            "controlGeneration": 0, "executionProtocol": 1,
        })
    );
}
