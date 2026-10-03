use super::*;

#[tokio::test]
async fn session_local_workspace_tasks_reach_acp_snapshot_and_live_events() {
    use peri_acp_types::ports::McpPoolPort;
    use peri_acp_types::tasks::{BgTaskKind, ExternalTaskRegistration};

    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().canonicalize().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_string_lossy().into_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    assert!(
        cfg.mcp_pool.is_none(),
        "host has no MCP task owner in this fixture"
    );
    let transport = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    let pool = sessions[id]
        .environment
        .as_ref()
        .unwrap()
        .cfg
        .mcp_pool
        .as_ref()
        .unwrap()
        .clone()
        .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
        .ok()
        .unwrap();
    let initial_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if transport
            .notifications()
            .iter()
            .any(|(_, notice)| notice.to_string().contains("bg-task-snapshot"))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < initial_deadline,
            "missing initial task snapshot"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let session = cfg.session_manager.get_session(id).unwrap();
    let task_id = session
        .task_manager
        .register_external(ExternalTaskRegistration {
            session_id: id.to_owned(),
            owner_identity: "workspace-test".into(),
            owner_task_id: "shell-1".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 10".into(),
            started_at: None,
            cancel: Arc::new(|| Box::pin(async { Ok(()) })),
            on_terminal: Arc::new(|_, _| Ok(())),
        })
        .unwrap();
    assert!(
        McpPoolPort::has_active_tasks(pool.as_ref(), id),
        "the session MCP pool must hold the same TaskManager as ACP"
    );
    let snapshot = handle_request(
        "session/bg-tasks",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert!(snapshot["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|task| task["task_id"] == task_id && task["kind"] == "shell"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if transport.notifications().iter().any(|(_, notice)| {
            notice.to_string().contains("bg-task-started") && notice.to_string().contains(&task_id)
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "missing live ACP task event"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
