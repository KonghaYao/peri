use super::*;

/// 测试夹具：把一次未完成排空的装配保留在会话表里，供关闭重试。
///
/// 原为生产侧 fork 补偿链的保留入口；该补偿链已随 fork 门面迁移删除，这里按
/// 同一形态在测试内构造，继续锁定「Closing + 本实例运行句柄 + 资源保留」不变量。
fn retain_failed_assembly(
    sessions: &mut HashMap<String, SessionState>,
    id: &str,
    cwd: &str,
    owner: Arc<dyn peri_acp_types::workspace::SessionExecutionLease>,
    environment: Arc<crate::host::workspace::SessionEnvironment>,
) {
    sessions.insert(
        id.to_owned(),
        SessionState {
            session_id: id.to_owned(),
            thread_id: id.to_owned(),
            cwd: cwd.to_owned(),
            execution_owner: Some(owner),
            environment: Some(environment),
            closing: true,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );
}

#[tokio::test]
async fn worktree_failed_assembly_retains_resources_and_lease_until_cleanup_retry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let id = create_bound_fixture(&cfg, cwd.to_str().unwrap(), None).await;
    let owner = acquire_bound_owner(&cfg, &id).await;
    let retained_owner = owner.clone();
    let mut environment =
        crate::host::workspace::SessionEnvironment::assemble(&cfg, cwd.to_str().unwrap(), &id)
            .await
            .unwrap()
            .unwrap();
    let pool = Arc::new(RetryShutdownPool {
        settled: AtomicBool::new(false),
        called: AtomicBool::new(false),
    });
    Arc::get_mut(&mut environment).unwrap().cfg.mcp_pool = Some(pool.clone());
    assert!(!environment.shutdown().await);
    let mut sessions = HashMap::new();
    retain_failed_assembly(
        &mut sessions,
        &id,
        cwd.to_str().unwrap(),
        owner,
        environment,
    );
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    assert!(sessions[&id].closing);
    assert!(Arc::ptr_eq(
        sessions[&id].execution_owner.as_ref().unwrap(),
        &retained_owner,
    ));
    let close = json!({"sessionId": id});
    assert!(
        handle_request("session/close", &close, &cfg, &mut sessions, &transport)
            .await
            .is_err()
    );
    assert!(sessions.contains_key(&id));
    assert!(Arc::ptr_eq(
        sessions[&id].execution_owner.as_ref().unwrap(),
        &retained_owner,
    ));
    append_human_message(&cfg, &id, "retained runtime handle").await;
    // A closing owner cannot mutate session configuration or restart execution.
    assert!(handle_request(
        "session/set_mode",
        &json!({"sessionId": id, "modeId": "ask"}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .is_err());
    pool.settled.store(true, Ordering::Release);
    handle_request("session/close", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(!sessions.contains_key(&id));
    assert!(cfg
        .session_resources
        .append_history(&id, &[])
        .await
        .is_err());
}
