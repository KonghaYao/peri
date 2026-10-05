use super::*;

/// legacy 原始事实：只有 thread 行与消息，**没有 binding、没有 frozen**。
///
/// 用裸句柄按原表构造——门面没有「无 binding 无 frozen」的创建入口；`bridge` 与 `cfg`
/// 的门面出自同一次打开（同一库句柄），后续经协议/门面读到的是同一份事实。
async fn old_thread(bridge: &SqliteThreadStore, cwd: &Path) -> String {
    let id = bridge
        .create_thread(ThreadMeta::new_at(
            cwd.to_str().unwrap(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    bridge
        .append_message(
            &id,
            peri_acp_types::messages::BaseMessage::human("legacy user message"),
        )
        .await
        .unwrap();
    id
}

#[tokio::test]
#[serial]
async fn legacy_history_context_then_load_restores_saved_cwd_and_frozen_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("saved-project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), "LEGACY_PROJECT_INSTRUCTION").unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&bridge, &cwd).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let context = handle_request(
        "peri/session_context",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(context["workspace"]["cwd"], cwd.to_str().unwrap());
    assert!(context["binding"].is_null());
    assert!(bridge.load_session_binding(&id).await.unwrap().is_none());
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_none());
    handle_request(
        "session/load",
        &json!({"sessionId":id,"cwd":cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(Path::new(&sessions[&id].cwd), cwd);
    assert_eq!(sessions[&id].history[0].content(), "legacy user message");
    // W5（J2 §3.1）：legacy 首次接纳发生在内容准入之前，没有执行环境 ⇒ 没有
    // workspace 资源面 ⇒ 项目指令不可得（`claude_md` 为空），且**不回落磁盘**
    // （同样的 X4/J5 口径见 W4b 的 legacy 空技能摘要先例）。后半段
    // 「load 恢复已保存 frozen 快照」的断言不受影响。
    assert!(
        sessions[&id].frozen.as_ref().unwrap().claude_md().is_none(),
        "legacy 首次接纳：无资源面 ⇒ 指令不可得（零磁盘兜底）"
    );
    assert!(
        std::path::Path::new(&cwd).join("CLAUDE.md").is_file()
            || std::path::Path::new(&cwd).join("AGENTS.md").is_file(),
        "零兜底前提：磁盘上的指令文件仍在"
    );
    let frozen = bridge.load_frozen_snapshot(&id).await.unwrap().unwrap();
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), "CHANGED_LATER").unwrap();
    handle_request(
        "session/resume",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(
        bridge.load_frozen_snapshot(&id).await.unwrap().unwrap(),
        frozen
    );
    let fork = handle_request(
        "session/fork",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fork_id = fork["sessionId"].as_str().unwrap();
    assert_eq!(
        sessions[fork_id].history[0].content(),
        "legacy user message"
    );
    handle_request(
        "session/close",
        &json!({"sessionId":fork_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}

#[tokio::test]
#[serial]
async fn legacy_history_missing_directory_is_readable_without_adoption() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let saved = tmp.path().join("removed");
    let id = old_thread(&bridge, &saved).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let response = handle_request(
        "peri/session_history",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(response["payloads"].as_array().unwrap().len(), 1);
    assert!(response["binding"].is_null());
    for method in ["session/load", "session/resume"] {
        assert!(handle_request(
            method,
            &json!({"sessionId":id,"cwd":tmp.path()}),
            &cfg,
            &mut sessions,
            &transport
        )
        .await
        .is_err());
        assert!(sessions.is_empty());
        assert!(cfg.session_manager.get_session(&id).is_none());
    }
    assert!(bridge.load_session_binding(&id).await.unwrap().is_none());
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn legacy_history_load_ignores_wrong_directory_and_preserves_saved_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let saved = root.join("saved");
    let other = root.join("other");
    std::fs::create_dir(&saved).unwrap();
    std::fs::create_dir(&other).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&bridge, &saved).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":id,"cwd":other}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(Path::new(&sessions[&id].cwd), saved);
    assert_eq!(sessions[&id].history[0].content(), "legacy user message");
    assert_eq!(Path::new(&bridge.load_meta(&id).await.unwrap().cwd), saved);
    assert!(bridge.load_session_binding(&id).await.unwrap().is_some());
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_some());
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}

#[tokio::test]
#[serial]
async fn legacy_history_rejects_bad_frozen_without_adoption() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let saved = std::fs::canonicalize(tmp.path()).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    for snapshot in ["broken", r#"{"version":999,"data":{}}"#] {
        let id = old_thread(&bridge, &saved).await;
        bridge
            .store_frozen_snapshot_if_absent(&id, snapshot)
            .await
            .unwrap();
        let error = handle_request(
            "session/load",
            &json!({"sessionId":id}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err();
        assert!(
            error.message.contains("frozen snapshot"),
            "{}",
            error.message
        );
        assert!(bridge.load_session_binding(&id).await.unwrap().is_none());
        assert_eq!(
            bridge.load_frozen_snapshot(&id).await.unwrap().as_deref(),
            Some(snapshot)
        );
        assert!(cfg.session_manager.get_session(&id).is_none());
    }
    assert!(sessions.is_empty());
}

#[tokio::test]
#[serial]
async fn legacy_history_fix_does_not_rebuild_missing_native_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    // 已绑定但**缺 frozen**：门面创建要求 frozen 成立，因此夹具按原表构造。
    let workspace = bridge.resolve_workspace(tmp.path()).await.unwrap();
    let id = bridge
        .create_bound_thread(
            ThreadMeta::new_at(workspace.cwd.to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let error = handle_request(
        "session/load",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "Bound session has no frozen snapshot");
    assert!(bridge.load_frozen_snapshot(&id).await.unwrap().is_none());
    assert!(sessions.is_empty());
}

#[tokio::test]
#[serial]
async fn legacy_history_freezes_saved_workspace_configuration_and_plugins() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("saved");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir_all(target.join(".peri/meta")).unwrap();
    let target = std::fs::canonicalize(&target).unwrap();
    std::fs::write(target.join(".peri/meta/01_intro.md"), "SAVED_META_HARNESS").unwrap();
    std::fs::write(
        target.join(".peri/settings.json"),
        r#"{"config":{"language":"zh-CN","meta_harness":{"01_intro":true,"WebMiddleware":false}}}"#,
    )
    .unwrap();
    seed_plugin_ecc(tmp.path());
    let skills = tmp.path().join(".claude/plugins/ecc/skills/legacy-skill");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: legacy-skill\ndescription: SAVED_PLUGIN_SKILL\n---\nLegacy plugin skill",
    )
    .unwrap();
    let mut config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    config.config.language = Some("en".into());
    let (mut cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    crate::provider::save_to(&config, cfg.config_source.global_path()).unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: startup.to_str().unwrap().to_owned(),
        bare: false,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    assert!(cfg.plugin_skill_roots.is_empty());
    let id = old_thread(&bridge, &target).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let frozen = sessions[&id].frozen.as_ref().unwrap().v2_frozen();
    assert_eq!(frozen.language.as_deref(), Some("zh-CN"));
    // J6/X8：legacy 首次接纳发生在执行环境（与 MCP 资源面）建立之前，覆盖文档不可得
    // ⇒ 保持内置段落、不回落磁盘读 `.peri/meta`（该目录仍在，但不得被宿主读取）。
    assert!(
        frozen.meta_harness.section_overrides.is_empty(),
        "legacy 接纳不得从磁盘读取段落覆盖"
    );
    assert!(frozen
        .meta_harness
        .disabled_middlewares
        .contains("WebMiddleware"));
    // W4b（F3/J5）：技能摘要同样只在内容准入期（P4）从 system 来源取；
    // legacy 首次接纳先于执行环境 ⇒ 无资源面 ⇒ 摘要为空。插件技能根在盘上
    // 存在也不得被宿主读取（宿主已无技能扫描点）。
    assert!(
        frozen.skill_summary.is_empty(),
        "legacy 接纳不得从磁盘/插件根读技能目录：{}",
        frozen.skill_summary
    );
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}
