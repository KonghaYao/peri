use super::*;

#[tokio::test]
async fn session_new_and_load_publish_task_snapshots_and_revisioned_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config.clone(), provider, &tmp).await;
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd":tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_owned();
    let first = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(data) = mock.notifications().iter().find_map(|(method, payload)| {
                (method == "peri/unstable_event" && payload["event"] == "bg-task-snapshot")
                    .then(|| payload["data"].clone())
            }) {
                break data;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(first["revision"], 0);
    let manager = cfg
        .session_manager
        .get_session(&sid)
        .unwrap()
        .task_manager
        .clone();
    manager
        .register(peri_acp_types::tasks::BgTaskRegistration {
            task_id: "wf-revision-1".into(),
            kind: BgTaskKind::Workflow,
            summary: "workflow revision".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let started = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(data) = mock.notifications().iter().find_map(|(method, payload)| {
                (method == "peri/unstable_event" && payload["event"] == "bg-task-started")
                    .then(|| payload["data"].clone())
            }) {
                break data;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(started["revision"], 1);
    assert_eq!(started["task_id"], "wf-revision-1");

    // 冷宿主接管执行前，原宿主须释放同一 Session 的持久 owner（与
    // `requests_workspace_cases_test` 的接管前置同款）：同一 session 不允许被
    // 两个活宿主同时持有，先 close（delete=false，不删数据）再 load。
    // close 要求会话任务终态且无未确认的外部执行：先取消上面注册的演示任务
    // （kill 为空操作 ⇒ 无真实执行），并确认该作用域确已排空（Kill 句柄无法
    // 自证清空，生产由工具层调用点在执行真停止后确认）。
    manager.cancel("wf-revision-1").unwrap();
    manager.confirm_external_execution_stopped("wf-revision-1");
    handle_request(
        "session/close",
        &json!({"sessionId": sid}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let second_cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let load_mock = Arc::new(MockTransport::default());
    let load_transport: Arc<dyn crate::transport::AcpTransport> = load_mock.clone();
    let mut loaded_sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":sid,"cwd":tmp.path()}),
        &second_cfg,
        &mut loaded_sessions,
        &load_transport,
    )
    .await
    .unwrap();
    let loaded = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(data) = load_mock
                .notifications()
                .iter()
                .find_map(|(method, payload)| {
                    (method == "peri/unstable_event" && payload["event"] == "bg-task-snapshot")
                        .then(|| payload["data"].clone())
                })
            {
                break data;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(loaded["revision"].is_u64());
    assert!(loaded["tasks"].is_array());
}

/// [回归测试] cancel-bg-task 对 Workflow 类型任务必须真正 kill（issue 2026-08-05）。
/// 历史 bug：Workflow 注册时固定 `Kill(None)`，cancel() 只 warn 并返回 success——
/// 条目移除但 runner 继续运行。修复后 kill 闭包（生产路径转发
/// WorkflowTaskRegistry::kill）随注册存入条目，cancel() 触发闭包。
/// 本测试用探针闭包在 RPC 层锁定该行为。

#[tokio::test]
async fn test_cancel_bg_task_workflow_invokes_kill_closure() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({"cwd":tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_owned();

    let killed = Arc::new(AtomicBool::new(false));
    let killed_clone = killed.clone();
    let registry = &cfg.session_manager.get_session(&sid).unwrap().task_manager;
    registry
        .register(peri_acp_types::tasks::BgTaskRegistration {
            task_id: "wf-run-1".to_string(),
            kind: BgTaskKind::Workflow,
            summary: "wf cancel test".to_string(),
            pid: None,
            kill: Some(Box::new(move || {
                killed_clone.store(true, Ordering::SeqCst);
            })),
        })
        .unwrap();
    assert_eq!(registry.active_count(), 1);

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": sid, "taskId": "wf-run-1" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(
        result.is_ok(),
        "取消 Workflow 任务应返回 success，实际: {:?}",
        result.err()
    );
    assert!(
        killed.load(Ordering::SeqCst),
        "cancel-bg-task 必须触发 kill 闭包（runner 真正被终止），而非仅移除条目"
    );
    assert_eq!(registry.active_count(), 0, "取消后条目应从 registry 移除");
}

/// [回归测试] cancel-bg-task 会话不存在时必须如实报错（issue 2026-08-05）。
/// 历史 bug：静默返回 success，掩盖"取消未生效"。
#[tokio::test]
async fn test_cancel_bg_task_session_not_found_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": "no-such-session", "taskId": "wf-run-1" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "会话不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "错误消息应提及 session not found，实际: {}",
        err.message
    );
}

/// [回归测试] cancel-bg-task 任务不存在时必须如实报错（issue 2026-08-05）。
/// 与 session_not_found 区分（错误消息不同），客户端可据此判断重试策略。
#[tokio::test]
async fn test_cancel_bg_task_task_not_found_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid = "cancel-bg-session".to_string();
    cfg.session_manager
        .new_session_with_id(&sid, tmp.path().to_str().unwrap())
        .await
        .unwrap();

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": sid, "taskId": "no-such-task" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "任务不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("not found"),
        "错误消息应提及 not found，实际: {}",
        err.message
    );
}

// ── workflow/kill_run & workflow/kill_agent sessionId 分发测试（issue 2026-08-05）──

/// Mock workflow executor（仅用于构造 WorkflowMiddleware，不真正执行 agent）
/// 在 middleware 的 registry 注册一个 Running 的 run（kill_tx 保持 open）。
fn register_run(mw: &Arc<WorkflowMiddleware>, run_id: &str) {
    let (kill_tx, _kill_rx) = tokio::sync::oneshot::channel::<()>();
    let child = tokio::spawn(async {});
    mw.registry()
        .register(WorkflowRun {
            run_id: run_id.to_string(),
            workflow_name: "wf-test".to_string(),
            script_preview: "test".to_string(),
            status: WorkflowRunStatus::Running,
            started_at: std::time::Instant::now(),
            child_handle: Some(child),
            kill_tx: Some(kill_tx),
        })
        .unwrap();
}

/// [回归测试] workflow/kill_run 必须按请求 sessionId 定位 session（issue 2026-08-05）。
/// 历史 bug：`sessions.values().find_map()` 取第一个带 middleware 的 session，
/// 多 session 时可能 kill 错 session（run 在另一 session 却报 killed:true）。
#[tokio::test]
async fn test_kill_run_targets_requested_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    let mw_a = register_session_with_workflow(&mut sessions, "sess-a", cwd, &cfg).await;
    let mw_b = register_session_with_workflow(&mut sessions, "sess-b", cwd, &cfg).await;
    register_run(&mw_a, "run-a");
    register_run(&mw_b, "run-b");

    // run-a 只在 sess-a：请求 sess-b 杀 run-a 必须 killed:false（修复前可能误报 true）
    let resp = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": "sess-b", "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], false, "sess-b 无 run-a，不得误报 killed");

    // 请求 sess-b 杀 run-b → killed:true，且只影响 sess-b 的 registry
    let resp = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": "sess-b", "runId": "run-b" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], true, "sess-b 的 run-b 应被 kill");
    assert!(
        mw_b.registry().list_runs().is_empty(),
        "sess-b 的 registry 应已移除 run-b"
    );
    assert!(
        !mw_a.registry().list_runs().is_empty(),
        "sess-a 的 registry 不得受影响"
    );

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前静默返回 killed:false）
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": "no-such-session", "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );

    // session 存在但无 workflow middleware → 明确错误
    let sid = register_session_with_history(&mut sessions, cwd, &cfg).await;
    let err = handle_request(
        "workflow/kill_run",
        &json!({ "sessionId": sid, "runId": "run-a" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "无 middleware 的会话应报错，实际: {}",
        err.message
    );
}

/// [回归测试] workflow/kill_agent 必须按请求 sessionId 定位 session（issue 2026-08-05）。
/// 深层 kill 依赖 runner 内部 active_channels（外部不可注入），此处锁定协议层：
/// 缺失/不存在的 session 如实报错，存在的 session 正常返回 killed 结果。
#[tokio::test]
async fn test_kill_agent_targets_requested_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    register_session_with_workflow(&mut sessions, "sess-a", cwd, &cfg).await;
    register_session_with_workflow(&mut sessions, "sess-b", cwd, &cfg).await;

    // 存在 session：正常返回 killed（sess-b 无该 run 的 active channel → false，不报错）
    let resp = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": "sess-b", "runId": "run-x", "agentId": 1 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(resp["killed"], false);

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/kill_agent",
        &json!({ "runId": "run-x", "agentId": 1 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前静默返回 killed:false）
    let err = handle_request(
        "workflow/kill_agent",
        &json!({ "sessionId": "no-such-session", "runId": "run-x", "agentId": 1 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );
}

/// [回归测试] workflow/resume 必须按请求 sessionId 定位 session（issue 2026-08-05）。
/// 历史 bug：`sessions.values().find_map()` 取第一个带 middleware 的 session，
/// 多 session 时可能 resume 错 session（与 kill_run 同源）。
#[tokio::test]
async fn test_resume_targets_requested_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp.path().to_str().unwrap();

    register_session_with_workflow(&mut sessions, "sess-a", cwd, &cfg).await;
    register_session_with_workflow(&mut sessions, "sess-b", cwd, &cfg).await;

    // 请求 sess-b + 不存在的 run → 错误来自 sess-b 的 middleware（read_state 失败），
    // 而非 "session not found"——证明分发到了 sess-b 而非第一个 session
    let err = handle_request(
        "workflow/resume",
        &json!({ "sessionId": "sess-b", "runId": "no-such-run" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("Failed to read workflow state"),
        "应分发到 sess-b 的 middleware 并报 read_state 失败，实际: {}",
        err.message
    );

    // 缺失 sessionId → -32602
    let err = handle_request(
        "workflow/resume",
        &json!({ "runId": "no-such-run" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报错，实际: {}",
        err.message
    );

    // session 不存在 → 明确错误（修复前可能误用第一个 session 的 middleware）
    let err = handle_request(
        "workflow/resume",
        &json!({ "sessionId": "no-such-session", "runId": "no-such-run" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "会话不存在应报 session not found，实际: {}",
        err.message
    );
}

// ── session/delete（标准 ACP，agentclientprotocol.com/protocol/v1/session-delete）──
