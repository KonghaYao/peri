use super::*;
use crate::transport::AcpTransport;
use peri_acp_types::tasks::ExternalCancelFn;

fn external_cancel_registration(
    session_id: &str,
    cancel: ExternalCancelFn,
) -> peri_acp_types::tasks::ExternalTaskRegistration {
    peri_acp_types::tasks::ExternalTaskRegistration {
        session_id: session_id.to_owned(),
        initiator_session_id: Some(session_id.to_owned()),
        owner_identity: "fixture".into(),
        owner_task_id: "remote-task".into(),
        kind: BgTaskKind::Mcp,
        summary: "pending cancel".into(),
        started_at: Some("2026-10-24T00:00:00Z".into()),
        cancel,
        on_terminal: Arc::new(|_, _| Box::pin(async { Ok(()) })),
    }
}

/// Real `ServerLoop` plus an in-memory transport; the sessions map is shared so
/// the test can observe lock scope while requests are in flight.
async fn start_cancel_server_loop(
    cfg: AcpServerConfig,
    states: HashMap<String, SessionState>,
) -> (
    Arc<crate::transport::mpsc::MpscClientTransport>,
    tokio::task::JoinHandle<()>,
) {
    let cfg = Arc::new(cfg);
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let client = Arc::new(client);
    let server: Arc<dyn crate::transport::AcpTransport> = Arc::new(server);
    let sessions = Arc::new(tokio::sync::Mutex::new(states));
    let prompt_locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (cont_tx, cont_rx) = tokio::sync::mpsc::unbounded_channel();
    let cont_tx = Arc::new(cont_tx);
    let connection = Arc::new(tokio::sync::Mutex::new(
        crate::host::connection::ConnectionContext::new(false),
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(async move {
        let _cont_rx = cont_rx;
        crate::host::server_loop::ServerLoop {
            transport: &server,
            cfg: &cfg,
            sessions: &sessions,
            prompt_locks: &prompt_locks,
            cont_tx: &cont_tx,
            connection: &connection,
            connection_cancellation: &cancellation,
        }
        .run()
        .await;
    });
    (client, task)
}

async fn new_cancel_fixture() -> (
    tempfile::TempDir,
    AcpServerConfig,
    HashMap<String, SessionState>,
    String,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut states = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({"cwd": tmp.path()}),
        &cfg,
        &mut states,
        &transport,
    )
    .await
    .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_owned();
    (tmp, cfg, states, sid)
}

/// A cancel that never resolves must still produce a bounded ACP failure, and
/// the task must stay unsettled (no fabricated terminal state).
#[tokio::test]
async fn cancel_bg_task_timeout_returns_error_and_keeps_task_unsettled() {
    let (_tmp, cfg, states, sid) = new_cancel_fixture().await;
    let manager = cfg
        .session_manager
        .get_session(&sid)
        .unwrap()
        .task_manager
        .clone();
    let ticket = manager
        .register_external(external_cancel_registration(
            &sid,
            Arc::new(|| Box::pin(std::future::pending::<Result<(), String>>())),
        ))
        .unwrap();
    let task_id = ticket.task_id().to_owned();
    let (client, loop_task) = start_cancel_server_loop(cfg, states).await;
    // The fixture opens SQLite before the clock is paused; auto-advance then
    // reaches the ACP cancel deadline without a real 10-second wait.
    tokio::time::pause();

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client.send_request(
            "session/cancel-bg-task",
            json!({"sessionId": sid, "taskId": task_id}),
        ),
    )
    .await
    .expect("cancel-bg-task must return within its ACP deadline")
    .unwrap_err();
    assert!(
        error.message.contains("timed out"),
        "cancel failure must be identifiable, got: {}",
        error.message
    );
    assert!(
        manager.has_unsettled_external(),
        "a cancel timeout must not settle the external task"
    );
    let snapshot = manager.snapshot();
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .find(|task| task.task_id == task_id)
            .unwrap()
            .status,
        "running",
        "cancel timeout must not fabricate a terminal state"
    );

    client.close();
    tokio::time::timeout(std::time::Duration::from_secs(2), loop_task)
        .await
        .expect("transport close must end the server loop")
        .unwrap();
}

/// While one cancel is pending, unrelated control requests must still be
/// served: the receive loop is not blocked and the sessions lock is released.
#[tokio::test]
async fn pending_cancel_does_not_block_control_requests() {
    let (_tmp, cfg, states, sid) = new_cancel_fixture().await;
    let manager = cfg
        .session_manager
        .get_session(&sid)
        .unwrap()
        .task_manager
        .clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let ticket = manager
        .register_external(external_cancel_registration(
            &sid,
            Arc::new({
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                move || {
                    let entered = Arc::clone(&entered);
                    let release = Arc::clone(&release);
                    Box::pin(async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    })
                }
            }),
        ))
        .unwrap();
    let task_id = ticket.task_id().to_owned();
    let (client, loop_task) = start_cancel_server_loop(cfg, states).await;

    let cancel_request = tokio::spawn({
        let client = Arc::clone(&client);
        let sid = sid.clone();
        let task_id = task_id.clone();
        async move {
            client
                .send_request(
                    "session/cancel-bg-task",
                    json!({"sessionId": sid, "taskId": task_id}),
                )
                .await
        }
    });
    entered.notified().await;

    let bg_tasks = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client.send_request("session/bg-tasks", json!({"sessionId": sid})),
    )
    .await
    .expect("session/bg-tasks must not wait for the pending cancel")
    .unwrap();
    assert_eq!(bg_tasks["tasks"][0]["task_id"], task_id);

    let snapshot = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client.send_request("session/input/snapshot", json!({"sessionId": sid})),
    )
    .await
    .expect("session/input/snapshot must not wait for the pending cancel")
    .unwrap();
    let generation = snapshot["generation"].as_str().unwrap().to_owned();
    let enqueued = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client.send_request(
            "session/input/enqueue",
            json!({
                "sessionId": sid,
                "generation": generation,
                "commandId": "enqueue-while-cancelling",
                "inputId": "00000000-0000-0000-0000-000000000001",
                "content": "queued while cancelling",
                "originalDraft": "queued while cancelling",
            }),
        ),
    )
    .await
    .expect("session/input/enqueue must not wait for the pending cancel")
    .unwrap();
    assert!(
        enqueued["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "enqueue must return a receipt while the cancel is pending: {enqueued}"
    );

    client
        .send_notification("session/cancel", json!({"sessionId": sid}))
        .await
        .unwrap();
    let after_cancel_notification = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client.send_request("session/bg-tasks", json!({"sessionId": sid})),
    )
    .await
    .expect("session/cancel notification must be consumed without blocking the loop")
    .unwrap();
    assert_eq!(after_cancel_notification["source"], "live");
    assert!(
        !cancel_request.is_finished(),
        "the pending cancel must still be in flight while other requests are served"
    );

    release.notify_one();
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(1), cancel_request)
        .await
        .expect("released cancel must complete")
        .unwrap()
        .unwrap();
    assert_eq!(cancelled["success"], true);

    client.close();
    tokio::time::timeout(std::time::Duration::from_secs(2), loop_task)
        .await
        .expect("transport close must end the server loop")
        .unwrap();
}
