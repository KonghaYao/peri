use super::*;

// ── 批 3 Step 5 迁移：MCP 发现预热 ────────────────────────────────────────────
//
// 原断言经 `run_acp_server_with_sessions`（外部注入共享 session map）驱动
// 统一路径：wire 请求 + map 内窥，语义与迁移前（handler 直调 + StdioContext
// 内窥）等价。`test_delete_removes_thread_*` 与 prewarm smoke 的 load 变体
// 已在 `host/requests_test.rs` 有等价覆盖，不重复迁移。

/// session/fork 分支：wire 形态下新 session 注册、history 复制语义与持久化一致。
#[tokio::test]
async fn test_fork_registers_session_and_copies_history() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp).await;
    let session_resources = Arc::clone(&cfg.session_resources);
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    // 前置：注册带非空 canonical history 的 source session。
    let source_message = peri_acp_types::messages::BaseMessage::human("hello");
    let source_message_id = source_message.id();
    let source_payload = peri_acp_types::store::PersistedPayload::Message(source_message.clone());
    let source_thread_id = "fork-source-session".to_string();
    create_bound_thread_fixture(&cfg, &source_thread_id, tmp.path().to_str().unwrap()).await;
    cfg.session_manager.ensure_session(
        &source_thread_id,
        std::fs::canonicalize(tmp.path()).unwrap().to_str().unwrap(),
    );
    cfg.session_resources
        .append_history(&source_thread_id, std::slice::from_ref(&source_payload))
        .await
        .unwrap();
    let source_frozen = cfg
        .session_manager
        .build_frozen_data(tmp.path().to_str().unwrap());
    sessions.lock().await.insert(
        "fork-source-session".to_string(),
        crate::host::SessionState {
            session_id: "fork-source-session".to_string(),
            thread_id: source_thread_id,
            cwd: std::fs::canonicalize(tmp.path())
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            environment: None,
            closing: false,
            history: vec![source_message],
            history_payloads: vec![source_payload],
            cancel_token: None,
            frozen: Some(source_frozen),
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
        },
    );
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/fork",
        json!({ "sessionId": "fork-source-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;
    let forked_id = result["sessionId"]
        .as_str()
        .expect("fork 响应应含新 sessionId: {result}")
        .to_string();
    assert_ne!(forked_id, "fork-source-session");

    let sessions = sessions.lock().await;
    let forked = sessions.get(&forked_id).expect("fork 应注册新 session");
    assert_eq!(forked.history_payloads.len(), 1);
    let forked_message_id = forked.history_payloads[0].id();
    assert_ne!(forked_message_id, source_message_id);
    assert_eq!(
        forked
            .history
            .first()
            .expect("fork history projection")
            .id(),
        forked_message_id,
        "fork SessionState 必须采用持久化复制后的新消息 ID"
    );
    let stored_fork = session_resources
        .load_session_snapshot(&forked_id)
        .await
        .unwrap()
        .payloads;
    assert_eq!(stored_fork.len(), 1);
    assert_eq!(stored_fork[0].id(), forked_message_id);
    drop(sessions);
    await_server_exit(server_task, input_write).await;
}

/// session/new 预热 MCP skill 发现 smoke：pool 已初始化但无已连接 server
/// 时 prewarm 空跑不 panic、响应正常（已连接 server 的发现行为由 middleware
/// 层单测覆盖）。
#[tokio::test]
async fn test_new_prewarms_mcp_discovery_smoke() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_empty_mcp_pool(&tmp).await;
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/new",
        json!({ "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;
    assert!(
        result["sessionId"].as_str().is_some_and(|s| !s.is_empty()),
        "session/new 应返回 sessionId: {result}"
    );
    // prewarm 空跑路径（已初始化空 pool）不 panic

    await_server_exit(server_task, input_write).await;
}
