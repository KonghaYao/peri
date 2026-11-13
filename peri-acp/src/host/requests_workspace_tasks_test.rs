use super::*;

/// Records the workspace watcher admitted by `bind_session_tasks`.
struct WatchRecordingPool {
    starts: std::sync::atomic::AtomicUsize,
    sessions: parking_lot::Mutex<Vec<String>>,
    cancel: parking_lot::Mutex<Option<tokio_util::sync::CancellationToken>>,
}

impl WatchRecordingPool {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: std::sync::atomic::AtomicUsize::new(0),
            sessions: parking_lot::Mutex::new(Vec::new()),
            cancel: parking_lot::Mutex::new(None),
        })
    }
}

#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for WatchRecordingPool {
    async fn shutdown(&self) -> peri_acp_types::ports::McpPoolShutdownReport {
        peri_acp_types::ports::McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        json!({})
    }

    async fn watch_workspace_tasks(
        self: Arc<Self>,
        session_id: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.sessions.lock().push(session_id.to_owned());
        *self.cancel.lock() = Some(cancel.clone());
        cancel.cancelled().await;
    }
}

/// `bind_session_tasks` is the session-entry seam used by recovery: it must
/// start the Workspace watcher once, and session close must cancel that watch.
#[tokio::test]
async fn session_entry_starts_workspace_watch_once_and_close_stops_it() {
    use peri_acp_types::ports::McpPoolPort;

    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().canonicalize().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    assert!(cfg.mcp_pool.is_none(), "fixture has no host MCP pool");
    let pool = WatchRecordingPool::new();
    cfg.mcp_pool = Some(Arc::clone(&pool) as Arc<dyn McpPoolPort>);

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

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while pool.starts.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session entry must start the workspace watcher");
    assert_eq!(pool.sessions.lock().as_slice(), [id.to_owned()]);

    // Re-entering the same session runtime must not admit a second watcher.
    bind_session_tasks(id, &cfg, &transport_dyn);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        pool.starts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "repeated binding must not duplicate the watcher"
    );

    let cancel = pool.cancel.lock().clone().expect("watcher token recorded");
    cfg.session_manager.pre_close_session(id);
    tokio::time::timeout(std::time::Duration::from_secs(2), cancel.cancelled())
        .await
        .expect("session close must stop the workspace watcher");
}

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
        capabilities: Default::default(),
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
    let ticket = session
        .task_manager
        .register_external(ExternalTaskRegistration {
            session_id: id.to_owned(),
            initiator_session_id: Some(id.to_owned()),
            owner_identity: "workspace-test".into(),
            owner_task_id: "shell-1".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 10".into(),
            started_at: None,
            cancel: Arc::new(|| Box::pin(async { Ok(()) })),
            on_terminal: Arc::new(|_, _| Box::pin(async { Ok(()) })),
        })
        .unwrap();
    let task_id = ticket.task_id().to_owned();
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
