//! 用户队列的控制请求、执行准入与 MPSC/stdio 契约。

use super::*;

fn make_user_input_session(
    tmp: &tempfile::TempDir,
) -> (AcpServerConfig, HashMap<String, SessionState>, String) {
    let config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "unused-test-key",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config, provider, tmp);
    let mut sessions = HashMap::new();
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
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
async fn test_user_input_methods_require_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp);
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
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp);
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
        cfg.session_manager.v2_queue_for(&sid).unwrap().is_empty(),
        "中断收尾前不能提前交 MQ"
    );
}

#[tokio::test]
async fn test_user_input_generation_invalidation_rejects_old_command() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp);
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
    cfg.session_manager.invalidate_user_input_mailbox(&sid);
    let request = make_user_input_request(
        &sid,
        old["generation"].as_str().unwrap(),
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
        data["snapshot"]["generation"], old["generation"],
        "恢复实例必须生成不同身份"
    );
    assert!(
        data["snapshot"]["items"].as_array().unwrap().is_empty(),
        "旧内容不能进入新实例"
    );
}

#[tokio::test]
async fn test_user_input_observer_can_read_but_cannot_mutate() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp);
    sessions[&sid].lease.release("default");
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
    assert_eq!(error.code, -32602, "观察者只允许读取快照");
    assert!(
        error.message.contains("read-only observer"),
        "错误必须明确指出写权限"
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
async fn test_user_input_stop_revokes_ticket_before_cancel_token_registration() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp);
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
    let ticket = mailbox.reserve_run().unwrap();
    crate::host::notify::handle_notification(
        "session/cancel",
        &json!({"sessionId":sid}),
        &mut sessions,
        &cfg,
    );
    assert!(
        !mailbox.attach_attempt(&ticket, tokio_util::sync::CancellationToken::new()),
        "Stop 后旧 ticket 不能进入执行"
    );
    assert_eq!(
        mailbox.snapshot().items[0].state,
        peri_acp_types::session::UserInputState::Queued,
        "尚未领取的输入恢复为可取回"
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
    let (cfg, mut states, sid) = make_user_input_session(&tmp);
    let cfg = Arc::new(cfg);
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let client = Arc::new(client);
    let server: Arc<dyn AcpTransport> = Arc::new(server);
    let mailbox = crate::host::user_input::ensure_mailbox(&sid, &cfg, &server).unwrap();
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
        reply["results"][0]["state"], "queued",
        "运行中普通输入只进入待发区"
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
async fn test_user_input_run_started_is_delivered_before_execution_can_continue() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp);
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
    handle_request(
        "session/input/enqueue",
        &make_user_input_request(
            &sid,
            initial["generation"].as_str().unwrap(),
            "00000000-0000-0000-0000-000000000001",
            "需要审批的工作",
        ),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let mailbox = cfg.session_manager.user_input_mailbox_for(&sid).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    assert!(mailbox.attach_attempt(&ticket, tokio_util::sync::CancellationToken::new()));
    crate::host::user_input::publish_run_started(&sid, &mailbox, &ticket, &cfg, &transport_dyn)
        .await
        .unwrap();
    let started: Vec<_> = transport
        .notifications()
        .into_iter()
        .filter_map(|(method, payload)| {
            if method != "peri/agent_event" {
                return None;
            }
            serde_json::from_str::<crate::event::AcpEvent>(payload["event_json"].as_str()?).ok()
        })
        .filter(|event| matches!(event, crate::event::AcpEvent::UserInputRunStarted { .. }))
        .collect();
    assert_eq!(
        started.len(),
        1,
        "await 返回前必须有且仅有一个启动通知写入 transport"
    );
    assert!(
        matches!(
            &started[0],
            crate::event::AcpEvent::UserInputRunStarted { generation, request_id }
                if generation == mailbox.generation() && request_id == &ticket.id
        ),
        "启动通知与后续 done 共享 ticket 身份"
    );
}

#[tokio::test]
async fn test_user_input_stdio_uses_same_short_control_requests() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_util::sync::CancellationToken;
    let tmp = tempfile::TempDir::new().unwrap();
    let (mut cfg, states, sid) = make_user_input_session(&tmp);
    cfg.stdio_command_filter = true;
    let cfg = Arc::new(cfg);
    let (mut input, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, output) = tokio::io::duplex(64 * 1024);
    let server: Arc<dyn crate::transport::AcpTransport> =
        Arc::new(crate::transport::stdio::StdioTransport::from_reader_writer(
            transport_read,
            transport_write,
        ));
    let mailbox = crate::host::user_input::ensure_mailbox(&sid, &cfg, &server).unwrap();
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
        response["result"]["results"][0]["state"], "queued",
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
    let (cfg, mut sessions, sid) = make_user_input_session(&tmp);
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
    let old = mailbox.reserve_run().unwrap();
    assert!(mailbox.attach_attempt(&old, CancellationToken::new()));
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
    let current = mailbox.reserve_run().unwrap();
    let current_cancel = CancellationToken::new();
    assert!(mailbox.attach_attempt(&current, current_cancel.clone()));
    sessions.get_mut(&sid).unwrap().cancel_token = Some(current_cancel.clone());
    crate::host::notify::handle_notification(
        "session/cancel",
        &json!({"sessionId":sid,"generation":generation,"requestId":old.id}),
        &mut sessions,
        &cfg,
    );
    assert!(
        !current_cancel.is_cancelled(),
        "旧 ticket 的迟到 Stop 不得取消新执行"
    );
    crate::host::notify::handle_notification(
        "session/cancel",
        &json!({"sessionId":sid,"generation":generation,"requestId":current.id}),
        &mut sessions,
        &cfg,
    );
    assert!(
        current_cancel.is_cancelled(),
        "当前 ticket 的 Stop 必须传到真实执行 token"
    );
}
