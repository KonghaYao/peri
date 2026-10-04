use super::*;

// ── 批 3 Step 5 迁移：host 级 LSP 池投影（H1 → H-04 A11/A22）与 MCP 发现预热 ──
//
// 原断言经 `run_acp_server_with_sessions`（外部注入共享 session map）驱动
// 统一路径：wire 请求 + map 内窥，语义与迁移前（handler 直调 + StdioContext
// 内窥）等价。`test_delete_removes_thread_*` 与 prewarm smoke 的 load 变体
// 已在 `host/requests_test.rs` 有等价覆盖，不重复迁移。
//
// H-04 改写（A11/A22 单 pool）：函数名保留（`spec/history/2026-09.md` 2026-09-10 条目:175
// 引用 `test_fork_creates_session_scoped_lsp_pool`），但语义从「分支**创建**会话级池」
// 改为「分支注册的 session **投影宿主唯一 pool 的同一 `Arc`**」——断言相应收紧为
// `Arc::ptr_eq`（仅 `is_some()` 不足以证伪「又建了一份池」）。

/// session/load 分支注册的 session 必须投影宿主唯一 LSP 池（同一 `Arc`）。
#[tokio::test]
async fn test_load_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_lsp(&tmp, vec![make_lsp_config()]).await;
    // 断言用的宿主句柄：`cfg` 随后被 move 进 server task。
    let host_pool = cfg
        .lsp_pool
        .clone()
        .expect("生产同构：空配置也构造 host pool");
    create_bound_thread_fixture(&cfg, "load-test-session", tmp.path().to_str().unwrap()).await;
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
        "session/load",
        json!({ "sessionId": "load-test-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;
    assert!(
        result["modes"].is_object() && result["configOptions"].is_array(),
        "load 响应应含 modes/configOptions: {result}"
    );

    let sessions = sessions.lock().await;
    let info = sessions
        .get("load-test-session")
        .expect("load 应注册 session");
    let pool = info.lsp_pool.as_ref().expect("session 必须投影 host pool");
    assert!(
        Arc::ptr_eq(pool, &host_pool),
        "load 分支的 session 必须投影宿主唯一 pool（同一 `Arc`），不得另建 pool"
    );
    drop(sessions);
    await_server_exit(server_task, input_write).await;
}

/// session/resume 分支（新 session）同上：投影宿主唯一 pool。
#[tokio::test]
async fn test_resume_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_lsp(&tmp, vec![make_lsp_config()]).await;
    let host_pool = cfg
        .lsp_pool
        .clone()
        .expect("生产同构：空配置也构造 host pool");
    create_bound_thread_fixture(&cfg, "resume-test-session", tmp.path().to_str().unwrap()).await;
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let _result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/resume",
        json!({ "sessionId": "resume-test-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;

    let sessions = sessions.lock().await;
    let info = sessions
        .get("resume-test-session")
        .expect("resume 应注册 session");
    let pool = info.lsp_pool.as_ref().expect("session 必须投影 host pool");
    assert!(
        Arc::ptr_eq(pool, &host_pool),
        "resume 分支的 session 必须投影宿主唯一 pool（同一 `Arc`），不得另建 pool"
    );
    drop(sessions);
    await_server_exit(server_task, input_write).await;
}

/// session/fork 分支创建的新 session 同样投影宿主唯一 pool（同一 `Arc`）。
#[tokio::test]
async fn test_fork_creates_session_scoped_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config_with_lsp(&tmp, vec![make_lsp_config()]).await;
    let host_pool = cfg
        .lsp_pool
        .clone()
        .expect("生产同构：空配置也构造 host pool");
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
    let workspace = cfg
        .session_resources
        .validate_bound_workspace(
            &source_thread_id,
            peri_acp_types::session_resources::BindingRecheck::Recorded,
        )
        .await
        .unwrap();
    let owner = cfg
        .session_resources
        .acquire_execution(&source_thread_id, &workspace)
        .await
        .unwrap();
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
            execution_owner: Some(owner),
            environment: None,
            closing: false,
            history: vec![source_message],
            history_payloads: vec![source_payload],
            cancel_token: None,
            frozen: Some(source_frozen),
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: None,
            lsp_pool: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            // 会话创建方即 writer（§6；测试源 session 同样建立 lease）
            lease: crate::host::lease::WriterLease::acquired("default"),
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
    let pool = forked
        .lsp_pool
        .as_ref()
        .expect("session 必须投影 host pool");
    assert!(
        Arc::ptr_eq(pool, &host_pool),
        "fork 分支的 session 必须投影宿主唯一 pool（同一 `Arc`），不得另建 pool"
    );
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

/// 无 LSP 配置（`plugin_lsp_servers = []`）时：host pool **仍然存在**（空配置池，
/// A6/A21：可见但空——`has_servers()` 为假 ⇒ `lsp` 实例工具面为空表、链上不装
/// 同步中间件），session 投影同一 `Arc`。
///
/// H-04 改写：原断言为「无配置时 `lsp_pool.is_none()`（不创建池）」，与新契约正面
/// 冲突——「不构造 pool」会让 `lsp` builtin 实例退化成「上下文缺失」而不是
/// 「可见但空」。函数名随之改为投影语义（原名 `test_load_without_lsp_config_has_no_pool`
/// 不再成立；该名未被任何文档引用）。
#[tokio::test]
async fn test_load_without_lsp_config_projects_empty_host_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp).await; // plugin_lsp_servers = []
    let host_pool = cfg.lsp_pool.clone().expect("空配置也必须构造 host pool");
    assert!(
        !peri_acp_types::ports::LspPoolPort::ready_for(
            host_pool.as_ref(),
            &tmp.path().join("probe.rs")
        ),
        "空配置 pool 对任意文件都不得报 ready（无 server 可路由）"
    );
    create_bound_thread_fixture(&cfg, "no-lsp-session", tmp.path().to_str().unwrap()).await;
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let sessions = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let server_task = tokio::spawn(host::run_acp_server_with_sessions(
        transport,
        cfg,
        sessions.clone(),
    ));

    send_initialize(&mut input_write, &mut output_read).await;
    let _result = send_request_and_read_result(
        &mut input_write,
        &mut output_read,
        2,
        "session/load",
        json!({ "sessionId": "no-lsp-session", "cwd": tmp.path().to_str().unwrap() }),
    )
    .await;

    let sessions = sessions.lock().await;
    let info = sessions.get("no-lsp-session").expect("load 应注册 session");
    let pool = info.lsp_pool.as_ref().expect("session 必须投影 host pool");
    assert!(
        Arc::ptr_eq(pool, &host_pool),
        "无 LSP 配置时 session 仍必须投影宿主唯一 pool（同一 `Arc`）"
    );
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
