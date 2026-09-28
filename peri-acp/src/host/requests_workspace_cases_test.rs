use super::*;

#[tokio::test]
async fn worktree_binding_hot_cold_resume_and_owner_are_consistent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("main");
    let linked = tmp.path().join("linked");
    std::fs::create_dir(&repo).unwrap();
    let run_git = |args: &[&std::ffi::OsStr]| {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init".as_ref(), "--quiet".as_ref()]);
    run_git(&[
        "-c".as_ref(),
        "user.name=Test".as_ref(),
        "-c".as_ref(),
        "user.email=test@example.invalid".as_ref(),
        "commit".as_ref(),
        "--allow-empty".as_ref(),
        "-m".as_ref(),
        "initial".as_ref(),
        "--quiet".as_ref(),
    ]);
    run_git(&[
        "worktree".as_ref(),
        "add".as_ref(),
        "--detach".as_ref(),
        linked.as_os_str(),
    ]);
    let sub = repo.join("src");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("CLAUDE.md"), "SOURCE_WORKTREE_FROZEN_SENTINEL").unwrap();
    let provider_config = make_provider_config("test", "openai", "key", "model");
    let peri_config = make_peri_config_with_provider(provider_config);
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config.clone(), provider.clone(), &tmp).await;
    let second = make_server_config(peri_config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": sub}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let original_cwd = std::fs::canonicalize(&sub).unwrap();
    assert_eq!(sessions[&id].cwd, original_cwd.to_str().unwrap());
    assert_eq!(
        created["_meta"]["peri.sessionWorkspaceV1"]["workspace"]["cwd"],
        original_cwd.to_str().unwrap()
    );
    let frozen = frozen_snapshot_bytes(&cfg, &id).await;
    append_human_message(&cfg, &id, "visible project session").await;
    for method in ["session/load", "session/resume", "session/fork"] {
        assert!(handle_request(
            method,
            &json!({"sessionId": id, "cwd": linked}),
            &cfg,
            &mut sessions,
            &transport
        )
        .await
        .is_err());
        assert_eq!(sessions[&id].cwd, original_cwd.to_str().unwrap());
    }
    let project = handle_request(
        "peri/session_context",
        &json!({"cwd": linked}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let list = handle_request("session/list", &json!({"_meta":{"peri.sessionWorkspaceV1":{"scope":{"kind":"project","value":project["workspace"]["project_id"]}}}}), &cfg, &mut sessions, &transport).await.unwrap();
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
    let exact = handle_request(
        "session/list",
        &json!({"cwd": linked}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(exact["sessions"].as_array().unwrap().is_empty());
    let forked = handle_request(
        "session/fork",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fork_id = forked["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(
        cfg.session_resources
            .load_session_binding(&fork_id)
            .await
            .unwrap(),
        cfg.session_resources
            .load_session_binding(&id)
            .await
            .unwrap()
    );
    assert_eq!(frozen_snapshot_bytes(&cfg, &fork_id).await, frozen);
    assert_eq!(own_payloads(&cfg, &fork_id).await.len(), 1);
    handle_request(
        "session/close",
        &json!({"sessionId": fork_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let mut cold = HashMap::new();
    // 热会话仍持有执行所有权：冷会话按只读准入进入，历史可读但不取得所有权。
    let read_only_load = handle_request(
        "session/load",
        &json!({"sessionId": id, "cwd": sub}),
        &second,
        &mut cold,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(
        read_only_load["_meta"]["peri.sessionWorkspaceV1"]["read_only"]["kind"],
        "peri.executionBusyV1"
    );
    assert!(cold[&id].execution_owner.is_none());
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    std::fs::write(sub.join("CLAUDE.md"), "MUTATED_AFTER_CLOSE").unwrap();
    handle_request(
        "session/resume",
        &json!({"sessionId": id}),
        &second,
        &mut cold,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(cold[&id].cwd, original_cwd.to_str().unwrap());
    assert_eq!(frozen_snapshot_bytes(&second, &id).await, frozen);
    assert!(cold[&id]
        .frozen
        .as_ref()
        .unwrap()
        .system_prompt()
        .contains(original_cwd.to_str().unwrap()));
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &second,
        &mut cold,
        &transport,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn worktree_missing_directory_history_is_read_only_and_load_is_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    append_human_message(&cfg, id, "saved history").await;
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    std::fs::remove_dir(&cwd).unwrap();
    let history = handle_request(
        "peri/session_history",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(history["payloads"].as_array().unwrap().len(), 1);
    assert!(sessions.is_empty());
    assert!(handle_request(
        "session/load",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport
    )
    .await
    .is_err());
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn worktree_new_resources_use_the_target_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("target");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("CLAUDE.md"), "TARGET_FROZEN_CONFIGURATION").unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config.clone(), provider, &tmp).await;
    crate::provider::save_to(&config, cfg.config_source.global_path()).unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: startup.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    let state = &sessions[id];
    let environment = state.environment.as_ref().unwrap();
    assert!(state
        .frozen
        .as_ref()
        .unwrap()
        .v2_frozen()
        .claude_md
        .contains("TARGET_FROZEN_CONFIGURATION"));
    assert_eq!(
        environment.cfg.session_manager.get_session(id).unwrap().cwd,
        state.cwd
    );
    let pool = environment
        .cfg
        .mcp_pool
        .as_ref()
        .expect("bare 会话仍提供 builtin workspace")
        .as_any()
        .downcast_ref::<peri_middlewares::mcp::McpClientPool>()
        .unwrap();
    let workspace = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(client) = pool.get_client("workspace") {
                if client.peer.is_some() && !client.tools.is_empty() {
                    break client;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bare workspace 必须完成初始化");
    use peri_agent::tools::{BaseTool, ToolContext};
    let read_tool = workspace
        .tools
        .iter()
        .find(|tool| tool.name == "Read")
        .unwrap();
    let read = peri_middlewares::mcp::tool_bridge::McpToolBridge::new(
        "workspace",
        read_tool,
        workspace.clone(),
    )
    .invoke(
        json!({"file_path": "CLAUDE.md"}),
        ToolContext::new(&[], &state.cwd),
    )
    .await
    .unwrap();
    assert!(read.contains("TARGET_FROZEN_CONFIGURATION"));
    assert!(environment.cfg.hook_groups.is_empty());
    assert_eq!(cfg.session_manager.get_session(id).unwrap().cwd, state.cwd);
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}

/// A resource that keeps closing until its external cleanup is acknowledged.
struct RetryShutdownPool {
    settled: AtomicBool,
    called: AtomicBool,
}

#[async_trait]
impl peri_acp_types::ports::McpPoolPort for RetryShutdownPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) -> peri_acp_types::ports::McpPoolShutdownReport {
        self.called.store(true, Ordering::Release);
        if self.settled.load(Ordering::Acquire) {
            peri_acp_types::ports::McpPoolShutdownReport::Complete {
                settled_services: 1,
                failed_services: 0,
            }
        } else {
            peri_acp_types::ports::McpPoolShutdownReport::Incomplete {
                settled_services: 0,
                unfinished_services: 1,
                failed_services: 0,
            }
        }
    }

    fn snapshot(&self) -> Value {
        json!({})
    }
}

/// 测试夹具：把一次未完成排空的装配保留在会话表里，供关闭重试。
///
/// 原为生产侧 fork 补偿链的保留入口；该补偿链已随 fork 门面迁移删除，这里按
/// 同一形态在测试内构造，继续锁定「Closing + 唯一 owner + 资源保留」不变量。
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
            lsp_pool: None,
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
    });
    let id = create_bound_fixture(&cfg, cwd.to_str().unwrap(), None).await;
    let owner = acquire_bound_owner(&cfg, &id).await;
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
    let competing = SqliteThreadStore::new(tmp.path().join("threads.db"))
        .await
        .unwrap();
    assert!(sessions[&id].closing);
    assert!(competing.acquire_execution_lease(&id).await.is_err());
    let close = json!({"sessionId": id});
    assert!(
        handle_request("session/close", &close, &cfg, &mut sessions, &transport)
            .await
            .is_err()
    );
    assert!(sessions.contains_key(&id));
    assert!(competing.acquire_execution_lease(&id).await.is_err());
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
    competing
        .acquire_execution_lease(&id)
        .await
        .unwrap()
        .mark_clean()
        .await
        .unwrap();
}

#[tokio::test]
async fn worktree_scheduled_approval_uses_session_permission_and_rejects_closed_owner() {
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
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    sessions[id]
        .environment
        .as_ref()
        .unwrap()
        .cfg
        .permission_mode
        .store(PermissionMode::Default);
    assert_eq!(cfg.permission_mode.load(), PermissionMode::Bypass);
    let sessions = Arc::new(tokio::sync::Mutex::new(sessions));
    let permission = crate::host::continuation::scheduled_permission_mode(&cfg, &sessions, id)
        .await
        .unwrap();
    assert_eq!(permission.load(), PermissionMode::Default);
    assert!(
        !crate::host::prompt::approve_scheduled_trigger(permission.as_ref(), None, "task", "run")
            .await
    );
    sessions.lock().await.get_mut(id).unwrap().closing = true;
    assert!(
        crate::host::continuation::scheduled_permission_mode(&cfg, &sessions, id)
            .await
            .is_err()
    );
    assert!(
        crate::host::continuation::scheduled_permission_mode(&cfg, &sessions, "missing")
            .await
            .is_err()
    );
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut *sessions.lock().await,
        &transport,
    )
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn worktree_session_end_retains_owner_and_joins_same_hook_on_retry() {
    use peri_acp_types::hooks::{HookEvent, RegisteredHook};
    use peri_acp_types::store::ThreadStore;

    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let target = target.canonicalize().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: target.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    let environment = sessions.get_mut(id).unwrap().environment.as_mut().unwrap();
    Arc::get_mut(environment).unwrap().cfg.hook_groups = vec![vec![RegisteredHook {
        hook: serde_json::from_value(json!({
            "type": "command",
            "command": "cat > ended.json; pwd > ended.cwd; echo end >> ended.count; while [ ! -f release ]; do sleep 0.02; done",
            "async": true,
            "timeout": 30,
        })).unwrap(),
        event: HookEvent::SessionEnd,
        matcher: None,
        plugin_name: "test".into(),
        plugin_id: "test".into(),
        plugin_root: target.clone(),
        plugin_data_dir: target.clone(),
        plugin_options: HashMap::new(),
    }]];
    let close = json!({"sessionId": id});
    let error = handle_request("session/close", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap_err();
    assert!(error.message.contains("incomplete"));
    let input: Value =
        serde_json::from_str(&std::fs::read_to_string(target.join("ended.json")).unwrap()).unwrap();
    assert_eq!(input["session_id"], id);
    assert_eq!(input["cwd"], target.to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(target.join("ended.cwd"))
            .unwrap()
            .trim(),
        target.to_str().unwrap()
    );
    let competing = SqliteThreadStore::new(tmp.path().join("threads.db"))
        .await
        .unwrap();
    assert!(competing
        .acquire_execution_lease(&id.to_owned())
        .await
        .is_err());
    std::fs::write(target.join("release"), "ready").unwrap();
    handle_request("session/close", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(!sessions.contains_key(id));
    assert_eq!(
        std::fs::read_to_string(target.join("ended.count")).unwrap(),
        "end\n"
    );
    competing
        .acquire_execution_lease(&id.to_owned())
        .await
        .unwrap()
        .mark_clean()
        .await
        .unwrap();
}

#[tokio::test]
async fn worktree_invalid_session_end_binding_skips_hook_but_drains_existing_resources() {
    use peri_acp_types::hooks::{HookEvent, RegisteredHook};
    use peri_acp_types::store::ThreadStore;

    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let target = target.canonicalize().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: target.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    let environment =
        Arc::get_mut(sessions.get_mut(id).unwrap().environment.as_mut().unwrap()).unwrap();
    environment.cfg.hook_groups = vec![vec![RegisteredHook {
        hook: serde_json::from_value(
            json!({"type":"command", "command":"echo unexpected > ended"}),
        )
        .unwrap(),
        event: HookEvent::SessionEnd,
        matcher: None,
        plugin_name: "test".into(),
        plugin_id: "test".into(),
        plugin_root: target.clone(),
        plugin_data_dir: target.clone(),
        plugin_options: HashMap::new(),
    }]];
    let pool = Arc::new(RetryShutdownPool {
        settled: AtomicBool::new(false),
        called: AtomicBool::new(false),
    });
    environment.cfg.mcp_pool = Some(pool.clone());
    let moved = tmp.path().join("moved");
    std::fs::rename(&target, &moved).unwrap();
    std::fs::create_dir(&target).unwrap();
    let close = json!({"sessionId": id});
    assert!(
        handle_request("session/close", &close, &cfg, &mut sessions, &transport)
            .await
            .is_err()
    );
    assert!(
        pool.called.load(Ordering::Acquire),
        "invalid hook binding must not prevent MCP shutdown"
    );
    assert!(!target.join("ended").exists());
    assert!(
        sessions.contains_key(id),
        "actual incomplete resource drain must retain the owner"
    );
    pool.settled.store(true, Ordering::Release);
    handle_request("session/close", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(!sessions.contains_key(id));
    assert!(
        !target.join("ended").exists(),
        "terminal hook must never run in a replacement directory"
    );
    std::fs::remove_dir(&target).unwrap();
    std::fs::rename(&moved, &target).unwrap();
    let competing = SqliteThreadStore::new(tmp.path().join("threads.db"))
        .await
        .unwrap();
    competing
        .acquire_execution_lease(&id.to_owned())
        .await
        .unwrap()
        .mark_clean()
        .await
        .unwrap();
}
