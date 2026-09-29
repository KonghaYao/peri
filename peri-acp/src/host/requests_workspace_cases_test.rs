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

/// frozen 同源（ARC-FROZEN-001 / J2 §6）：冷 load 的会话环境关闭集来自**持久 blob**
/// 的投影，而不是当轮配置的投影。
///
/// 构造方式是把配置改成相反取值：创建时 `WorkspaceMiddleware=false`（冻结记录关闭），
/// 重启时配置为 `true`（当轮投影不再关闭）。若装配仍在本地重建 frozen，冷 load 的关闭
/// 集就会跟着当轮配置变空，本用例的相等断言会失败。
#[tokio::test]
async fn cold_load_derives_builtin_closed_set_from_persisted_frozen() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = std::fs::canonicalize(tmp.path()).unwrap();
    let cwd = cwd.to_str().unwrap().to_owned();
    let provider_config = make_provider_config("test", "openai", "key", "model");
    let mut frozen_off = make_peri_config_with_provider(provider_config.clone());
    frozen_off.config.meta_harness =
        Some(HashMap::from([("WorkspaceMiddleware".to_string(), false)]));
    let provider = LlmProvider::from_config(&frozen_off).unwrap();
    let mut cfg = make_server_config(frozen_off, provider.clone(), &tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.clone(),
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
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let persisted = match cfg
        .session_resources
        .load_session_snapshot(&id)
        .await
        .unwrap()
        .frozen
    {
        peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes.into_string(),
        other => panic!("created session must carry a frozen snapshot: {other:?}"),
    };
    let persisted_frozen =
        crate::session::frozen_snapshot::decode_frozen_snapshot(&persisted).unwrap();
    assert!(
        persisted_frozen
            .meta_harness()
            .disabled_middlewares
            .contains("WorkspaceMiddleware"),
        "前提：冻结快照必须记录创建时的关闭项"
    );
    append_human_message(&cfg, &id, "existing history").await;
    handle_request(
        "session/close",
        &json!({"sessionId": id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    drop(cfg);
    drop(sessions);

    // 重启时把同一个键改成相反取值。当轮配置投影为空，持久 blob 投影仍含该关闭项。
    let mut frozen_on = make_peri_config_with_provider(provider_config);
    frozen_on.config.meta_harness =
        Some(HashMap::from([("WorkspaceMiddleware".to_string(), true)]));
    let mut restarted = make_server_config(frozen_on, provider, &tmp).await;
    restarted.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.clone(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    let mut restored = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId": id, "cwd": cwd}),
        &restarted,
        &mut restored,
        &transport,
    )
    .await
    .unwrap();

    let environment = restored[&id]
        .environment
        .as_ref()
        .expect("冷恢复必须重新装配会话环境");
    let expected = peri_middlewares::assembly::builtin_closed_instances(
        &persisted_frozen.meta_harness().disabled_middlewares,
    );
    assert_eq!(
        environment.builtin_closed(),
        &expected,
        "冷恢复的关闭集必须等于持久 blob 的投影（而不是当轮配置）"
    );
    assert!(
        !environment.builtin_closed().is_empty(),
        "哨兵：当轮配置为 true，若装配按当前状态重冻，这里会是空集"
    );
}

/// 补偿顺序（J2 §5.1）：`abandon` 只在环境排空**确认之后**发生。
///
/// 排空未确认时草稿必须原样保留（否则资源持有的是已删除会话的 lease/handle），
/// 重试排空成功后才撤销。
#[tokio::test]
async fn unpublished_draft_is_abandoned_only_after_confirmed_drain() {
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
    // 草稿：身份/绑定/执行代际成立，frozen 未提交。
    let workspace = cfg.session_resources.resolve_workspace(&cwd).await.unwrap();
    let id = new_session_id();
    let draft = bound_draft(&id, &workspace);
    let initialization = cfg
        .session_resources
        .begin_initialization(&draft)
        .await
        .unwrap();

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

    // 排空未确认：不撤销（草稿与执行代际保持原样）。
    let error =
        super::super::session_lifecycle::drain_and_abandon(Some(&environment), &initialization)
            .await
            .unwrap_err();
    assert!(error.message.contains("did not confirm shutdown"));
    assert!(pool.called.load(Ordering::Acquire), "必须真的尝试过排空");
    let snapshot = cfg
        .session_resources
        .load_session_snapshot(&id)
        .await
        .expect("草稿必须保留（判据未结清）");
    assert_eq!(
        snapshot.frozen,
        peri_acp_types::session_resources::FrozenState::LegacyAbsent,
        "未提交的草稿仍是 frozen 空"
    );

    // 排空确认之后才撤销。
    pool.settled.store(true, Ordering::Release);
    super::super::session_lifecycle::drain_and_abandon(Some(&environment), &initialization)
        .await
        .unwrap();
    assert!(
        cfg.session_resources
            .load_session_snapshot(&id)
            .await
            .is_err(),
        "撤销之后草稿行必须消失"
    );
}

/// 未发布创建（J2 第一阶段）的输入。
fn bound_draft(
    thread_id: &str,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
) -> peri_acp_types::session_resources::NewSessionDraft {
    peri_acp_types::session_resources::NewSessionDraft {
        thread_id: thread_id.to_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: peri_acp_types::session_resources::NewSessionMeta {
            title: None,
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: peri_acp_types::thread::CancelPolicy::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
    }
}

/// 同源收口（J2 §6.4）：无 frozen 的可执行会话 **fail-closed**，不按当前目录重冻。
///
/// 构造态：会话有执行所有权（`require_owner` 放行）但没有冻结快照。宿主必须拒绝本轮
/// 执行并报内部错误——任何「用当前状态补一份 frozen」的实现都会让本用例失败。
#[tokio::test]
async fn prompt_without_frozen_snapshot_fails_closed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config, provider, &tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    let id = create_bound_fixture(&cfg, cwd.to_str().unwrap(), None).await;
    let owner = acquire_bound_owner(&cfg, &id).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    sessions.insert(
        id.clone(),
        SessionState {
            session_id: id.clone(),
            thread_id: id.clone(),
            cwd: cwd.to_str().unwrap().to_owned(),
            execution_owner: Some(owner),
            environment: None,
            closing: false,
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

    // 执行入口需要 AcpSession 登记（user input mailbox 解析）：先按生产路径登记，
    // 但不给它 frozen —— 这正是本用例要触发的状态。
    cfg.session_manager
        .ensure_session_with_task_manager(&id, cwd.to_str().unwrap(), None);
    // 生产入口（`session/prompt` 的执行体）：同源守卫在这里，早于任何模型/工具路径。
    let error = crate::host::prompt::run_prompt(
        json!({
            "sessionId": id,
            "prompt": [{"type": "text", "text": "hello"}],
        }),
        &Arc::new(tokio::sync::Mutex::new(sessions)),
        &cfg,
        &transport,
        Arc::new(parking_lot::Mutex::new(
            crate::session::agent_pool::AgentPool::new(),
        )),
        None,
        false,
        None,
    )
    .await
    .expect_err("没有 frozen 的可执行会话不得进入执行");
    assert!(
        error.message.contains("no frozen snapshot"),
        "必须是 fail-closed 的内部错误，而不是静默重冻: {error:?}"
    );
}

/// J6/W3b：new 会话启用段落覆盖但**覆盖不可得**时，冻结保持内置段落、且不回落磁盘。
///
/// 当前生产装配的 builtin `workspace` 实例尚未接资源 provider（provider 包文档明示
/// 「生产装配点不调用 `with_resources`」，属尚未接线的波次），因此 resources/list 无
/// `peri-meta://` 条目：宿主必须按 X8 保持内置并**不读** `.peri/meta`（scanner 已删）。
/// 覆盖正文本就存在的端到端（资源直接可得）证据见 provider 包 wire 用例。
#[tokio::test]
async fn new_session_meta_override_unavailable_keeps_builtin_and_never_reads_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("target");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir_all(target.join(".peri/meta")).unwrap();
    std::fs::write(target.join(".peri/meta/01_intro.md"), "DISK-META-OVERRIDE").unwrap();
    std::fs::write(
        target.join(".peri/settings.json"),
        r#"{"config":{"meta_harness":{"01_intro":true}}}"#,
    )
    .unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config.clone(), provider, &tmp).await;
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
    .expect("session/new（覆盖不可得）必须成功——X8 不阻塞创建");

    let id = created["sessionId"].as_str().unwrap();
    let frozen = sessions[id].frozen.as_ref().expect("创建必须发布 frozen");
    assert!(
        frozen.meta_harness().section_overrides.is_empty(),
        "覆盖不可得时必须保持内置（X8）"
    );
    assert!(
        !frozen.system_prompt().contains("DISK-META-OVERRIDE"),
        "宿主不得回落到磁盘读 `.peri/meta`（X8 零 FS 兜底）"
    );
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
