use super::*;

/// [回归测试] sessionless setup 更新同一 provider ID 后，现有会话实际使用新连接，
/// 保留独立 profile 与冻结 prompt，其他工作区的环境和缓存不变。
#[tokio::test]
#[serial]
async fn test_update_config_refreshes_existing_owner_environments() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("target");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir(&target).unwrap();
    std::fs::write(startup.join("CLAUDE.md"), "FROZEN_BEFORE_SETUP").unwrap();
    let mut config = make_peri_config_with_provider(make_provider_config(
        "same-id",
        "openai",
        "old-secret",
        "sonnet-model",
    ));
    config.config.providers[0].base_url = "https://old.example/v1".into();
    config.config.profiles.opus.model = Some("session-opus-model".into());
    config.config.profiles.opus.effort = "low".into();
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config.clone(), provider, &tmp).await;
    crate::provider::save_to(&config, cfg.config_source.global_path()).unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: startup.to_str().unwrap().to_owned(),
        bare: true,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let mut ids = Vec::new();
    for cwd in [&startup, &startup, &target] {
        let created = handle_request(
            "session/new",
            &json!({"cwd": cwd}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        ids.push(created["sessionId"].as_str().unwrap().to_owned());
    }
    handle_request(
        "session/set_config_option",
        &json!({"sessionId": ids[1], "configId": "model", "value": "opus"}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let frozen = ids
        .iter()
        .map(|id| {
            crate::session::frozen_snapshot::encode_frozen_snapshot(
                sessions[id].frozen.as_ref().unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    for id in &ids {
        let state = sessions.get_mut(id).unwrap();
        let provider = state
            .environment
            .as_ref()
            .unwrap()
            .cfg
            .provider
            .read()
            .clone();
        state
            .agent_pool
            .subagent_llm_cache
            .insert("seed".into(), Arc::from(provider.into_model()));
    }
    std::fs::write(startup.join("CLAUDE.md"), "CHANGED_AFTER_CREATION").unwrap();
    let mut updated = config.clone();
    updated.config.providers[0].api_key = "new-secret".into();
    updated.config.providers[0].base_url = "https://new.example/v1".into();
    updated.config.profiles.opus.model = Some("host-opus-model".into());
    updated.config.profiles.opus.effort = "high".into();
    handle_request(
        "session/update_config",
        &json!({"config": updated}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    for (index, id) in ids.iter().enumerate() {
        let state = &sessions[id];
        let environment = state.environment.as_ref().unwrap();
        let provider = environment.cfg.provider.read().clone();
        let LlmProvider::OpenAi {
            api_key,
            base_url,
            model,
            effort,
            ..
        } = &provider
        else {
            panic!("必须保持 OpenAI provider")
        };
        let same_owner = index < 2;
        assert_eq!(
            api_key,
            if same_owner {
                "new-secret"
            } else {
                "old-secret"
            }
        );
        assert_eq!(
            base_url,
            if same_owner {
                "https://new.example/v1"
            } else {
                "https://old.example/v1"
            }
        );
        assert_eq!(
            environment.cfg.peri_config.read().config.active_alias,
            if index == 1 { "opus" } else { "sonnet" }
        );
        if index == 1 {
            assert_eq!(model, "session-opus-model");
            assert_eq!(effort.as_deref(), Some("low"));
        }
        let prepared = provider
            .into_model()
            .prepare_request(&peri_model::ModelRequest::default())
            .unwrap();
        assert_eq!(
            prepared.endpoint().host_str(),
            Some(if same_owner {
                "new.example"
            } else {
                "old.example"
            })
        );
        assert_eq!(state.agent_pool.subagent_llm_cache.is_empty(), same_owner);
        assert_eq!(
            crate::session::frozen_snapshot::encode_frozen_snapshot(state.frozen.as_ref().unwrap())
                .unwrap(),
            frozen[index]
        );
    }
    for id in ids {
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
}

/// [回归测试] 无法解析 provider 时必须拒绝，不能发布配置却继续使用旧连接。
#[tokio::test]
async fn test_update_config_unusable_provider_leaves_state_unchanged() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("same", "openai", "valid", "old"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config.clone(), provider, &tmp).await;
    let mut updated = config.clone();
    updated.config.providers[0].api_key.clear();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "session/update_config",
        &json!({"config": updated}),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "active profile has no usable provider");
    assert_eq!(*cfg.peri_config.read(), config);
    assert_eq!(cfg.provider.read().model_name(), "old");
    assert!(!cfg.config_source.global_path().exists());
}

/// [回归测试] 保存失败必须返回 ACP 错误，且不发布新的运行时配置。
#[tokio::test]
async fn test_update_config_persistence_failure_leaves_state_unchanged() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("same", "openai", "valid", "old"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config.clone(), provider, &tmp).await;
    std::fs::create_dir(cfg.config_source.global_path()).unwrap();
    let mut updated = config.clone();
    updated.config.providers[0].models.sonnet = "new".into();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "session/update_config",
        &json!({"config": updated}),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32603);
    assert_eq!(error.message, "Failed to persist config");
    assert_eq!(*cfg.peri_config.read(), config);
    assert_eq!(cfg.provider.read().model_name(), "old");
}
