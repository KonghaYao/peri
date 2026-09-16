use super::*;

async fn old_thread(cfg: &AcpServerConfig, cwd: &Path) -> String {
    let id = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd.to_str().unwrap()))
        .await
        .unwrap();
    cfg.thread_store
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
    let cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&cfg, &cwd).await;
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
    assert!(cfg
        .thread_store
        .load_session_binding(&id)
        .await
        .unwrap()
        .is_none());
    assert!(cfg
        .thread_store
        .load_frozen_snapshot(&id)
        .await
        .unwrap()
        .is_none());
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
    assert!(sessions[&id].execution_owner.is_some());
    assert!(sessions[&id]
        .frozen
        .as_ref()
        .unwrap()
        .claude_md()
        .unwrap()
        .contains("LEGACY_PROJECT_INSTRUCTION"));
    let frozen = cfg
        .thread_store
        .load_frozen_snapshot(&id)
        .await
        .unwrap()
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
        cfg.thread_store
            .load_frozen_snapshot(&id)
            .await
            .unwrap()
            .unwrap(),
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
    let cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&cfg, &tmp.path().join("removed")).await;
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
    let error = handle_request(
        "session/load",
        &json!({"sessionId":id,"cwd":tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("unavailable"), "{}", error.message);
    assert!(sessions.is_empty());
    assert!(cfg
        .thread_store
        .load_session_binding(&id)
        .await
        .unwrap()
        .is_none());
    assert!(cfg
        .thread_store
        .load_frozen_snapshot(&id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[serial]
async fn legacy_history_rejects_wrong_directory_and_bad_frozen_without_adoption() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let saved = root.join("saved");
    let other = root.join("other");
    std::fs::create_dir(&saved).unwrap();
    std::fs::create_dir(&other).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let id = old_thread(&cfg, &saved).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let error = handle_request(
        "session/load",
        &json!({"sessionId":id,"cwd":other}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        error.message.contains("does not match"),
        "{}",
        error.message
    );
    assert!(cfg
        .thread_store
        .load_session_binding(&id)
        .await
        .unwrap()
        .is_none());
    assert!(cfg
        .thread_store
        .load_frozen_snapshot(&id)
        .await
        .unwrap()
        .is_none());
    for snapshot in ["broken", r#"{"version":999,"data":{}}"#] {
        let id = old_thread(&cfg, &saved).await;
        cfg.thread_store
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
        assert!(cfg
            .thread_store
            .load_session_binding(&id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            cfg.thread_store
                .load_frozen_snapshot(&id)
                .await
                .unwrap()
                .as_deref(),
            Some(snapshot)
        );
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
    let cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    let workspace = cfg
        .thread_store
        .resolve_workspace(tmp.path())
        .await
        .unwrap();
    let id = cfg
        .thread_store
        .create_bound_thread(ThreadMeta::new(workspace.cwd.to_str().unwrap()), &workspace)
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
    assert!(cfg
        .thread_store
        .load_frozen_snapshot(&id)
        .await
        .unwrap()
        .is_none());
    assert!(sessions.is_empty());
    cfg.thread_store
        .acquire_execution_lease(&id)
        .await
        .unwrap()
        .mark_clean()
        .await
        .unwrap();
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
    let mut cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    crate::provider::save_to(&config, cfg.config_source.global_path()).unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: startup.to_str().unwrap().to_owned(),
        bare: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    assert!(cfg.plugin_skill_roots.is_empty());
    let id = old_thread(&cfg, &target).await;
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
    assert_eq!(
        frozen.meta_harness.section_overrides["01_intro"].as_ref(),
        "SAVED_META_HARNESS"
    );
    assert!(frozen
        .meta_harness
        .disabled_middlewares
        .contains("WebMiddleware"));
    assert!(
        frozen.skill_summary.contains("**legacy-skill** [plugin]"),
        "{}",
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
