#[cfg(unix)]
#[tokio::test]
async fn worktree_session_end_retains_owner_and_joins_same_hook_on_retry() {
    use peri_acp_types::hooks::{HookEvent, RegisteredHook};

    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let target = target.canonicalize().unwrap();
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = crate::host::requests::tests::make_server_config(config, provider, &tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: target.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
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
        plugin_source: None,
        plugin_root: target.clone(),
        plugin_data_dir: target.clone(),
        plugin_options: HashMap::new(),
    }]];
    let retained_environment = sessions[id].environment.as_ref().unwrap().clone();
    let close = json!({"sessionId": id});
    let error = crate::host::requests::handle_request(
        "session/close",
        &close,
        &cfg,
        &mut sessions,
        &transport,
    )
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
    assert!(Arc::ptr_eq(
        sessions[id].environment.as_ref().unwrap(),
        &retained_environment,
    ));
    crate::host::requests::tests::append_human_message(&cfg, id, "waiting for session end").await;
    std::fs::write(target.join("release"), "ready").unwrap();
    crate::host::requests::handle_request("session/close", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(!sessions.contains_key(id));
    assert_eq!(
        std::fs::read_to_string(target.join("ended.count")).unwrap(),
        "end\n"
    );
    assert!(!cfg
        .session_resources
        .is_session_closing(&id.to_owned())
        .await
        .unwrap());
}

#[tokio::test]
async fn worktree_session_end_uses_saved_directory_and_drains_resources() {
    use peri_acp_types::hooks::{HookEvent, RegisteredHook};
    use peri_acp_types::store::ThreadStore;

    for directory_replaced in [false, true] {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let target = target.canonicalize().unwrap();
        let config = crate::host::requests::tests::make_peri_config_with_provider(
            crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
        );
        let provider = LlmProvider::from_config(&config).unwrap();
        let mut cfg =
            crate::host::requests::tests::make_server_config(config, provider, &tmp).await;
        cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
            startup_cwd: target.to_str().unwrap().to_owned(),
            bare: true,
            drive_cron_tick: false,
            mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
            capabilities: Default::default(),
        });
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let mut sessions = HashMap::new();
        let created = crate::host::requests::handle_request(
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
            plugin_source: None,
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
        if directory_replaced {
            std::fs::create_dir(&target).unwrap();
        }
        let close = json!({"sessionId": id});
        assert!(crate::host::requests::handle_request(
            "session/close",
            &close,
            &cfg,
            &mut sessions,
            &transport
        )
        .await
        .is_err());
        assert!(
            pool.called.load(Ordering::Acquire),
            "incomplete resource drain must not prevent MCP shutdown"
        );
        assert!(!moved.join("ended").exists());
        if directory_replaced {
            // 同路径换成新对象后 cwd 仍可用：准入通过，SessionEnd 在保存目录执行。
            // hook 是异步收尾：close 可能在等待窗口内返回并保留任务（由客户端
            // 重试 join），因此这里等文件副作用，而不是假定 close 返回时已落盘。
            let ended = target.join("ended");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !ended.exists() && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert!(
                ended.exists(),
                "saved cwd 可用时 SessionEnd 必须在保存目录执行"
            );
        } else {
            assert!(!target.join("ended").exists());
        }
        assert!(
            sessions.contains_key(id),
            "actual incomplete resource drain must retain the owner"
        );
        pool.settled.store(true, Ordering::Release);
        crate::host::requests::handle_request(
            "session/close",
            &close,
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        assert!(!sessions.contains_key(id));
        assert!(
            !moved.join("ended").exists(),
            "terminal hook must never run in the moved directory"
        );
        if directory_replaced {
            assert!(target.join("ended").exists());
        } else {
            assert!(
                !target.join("ended").exists(),
                "terminal hook must not run while the saved directory is missing"
            );
        }
        if directory_replaced {
            std::fs::remove_dir_all(&target).unwrap();
        }
        std::fs::rename(&moved, &target).unwrap();
        let reopened = SqliteThreadStore::try_new(tmp.path().join("threads.db"))
            .await
            .unwrap();
        assert!(reopened.load_meta(&id.to_owned()).await.is_ok());
    }
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
    let provider_config =
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model");
    let mut frozen_off =
        crate::host::requests::tests::make_peri_config_with_provider(provider_config.clone());
    frozen_off.config.meta_harness =
        Some(HashMap::from([("workspace".to_string(), false)]));
    let provider = LlmProvider::from_config(&frozen_off).unwrap();
    let mut cfg =
        crate::host::requests::tests::make_server_config(frozen_off, provider.clone(), &tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.clone(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
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
            .contains("workspace"),
        "前提：冻结快照必须记录创建时的关闭项"
    );
    crate::host::requests::tests::append_human_message(&cfg, &id, "existing history").await;
    crate::host::requests::handle_request(
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
    let mut frozen_on =
        crate::host::requests::tests::make_peri_config_with_provider(provider_config);
    frozen_on.config.meta_harness =
        Some(HashMap::from([("workspace".to_string(), true)]));
    let mut restarted =
        crate::host::requests::tests::make_server_config(frozen_on, provider, &tmp).await;
    restarted.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.clone(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let mut restored = HashMap::new();
    crate::host::requests::handle_request(
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
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = crate::host::requests::tests::make_server_config(config, provider, &tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    // 草稿：身份/绑定/执行代际成立，frozen 未提交。
    let workspace = cfg.session_resources.resolve_workspace(&cwd).await.unwrap();
    let id = crate::host::requests::tests::new_session_id();
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
/// 构造态：已发布会话没有冻结快照。宿主必须拒绝本轮
/// 执行并报内部错误——任何「用当前状态补一份 frozen」的实现都会让本用例失败。
#[tokio::test]
async fn prompt_without_frozen_snapshot_fails_closed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = crate::host::requests::tests::make_server_config(config, provider, &tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    let id =
        crate::host::requests::tests::create_bound_fixture(&cfg, cwd.to_str().unwrap(), None).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    sessions.insert(
        id.clone(),
        SessionState {
            session_id: id.clone(),
            thread_id: id.clone(),
            cwd: cwd.to_str().unwrap().to_owned(),
            environment: None,
            closing: false,
            close_in_flight: 0,
            context_ownership: crate::host::ContextOwnership::Resident,
            history_payloads: Vec::new(),
            structure_version: 0,
            session_generation: crate::host::next_session_generation(),
            history_window: Default::default(),
            live_delivery: None,
            event_health: Default::default(),
            cancel_token: None,
            interaction_gate: Arc::new(tokio::sync::Mutex::new(())),
            host_request_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            recovery_settlement_pending: false,
            view_only: false,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            title: None,
            tags: Vec::new(),
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
