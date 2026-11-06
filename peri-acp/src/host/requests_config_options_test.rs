use super::super::*;
use crate::host::requests::config_options::{handle_set_config_option, handle_update_config};
use crate::session::agent_pool::{AgentPool, CachedLlmInstances};

#[derive(Clone, Copy)]
enum PersistenceFailure {
    Conflict,
    Io,
}

fn live_config() -> PeriConfig {
    let mut config = make_peri_config_with_provider(make_provider_config(
        "configured",
        "openai",
        "valid-key",
        "old-model",
    ));
    config.config.providers[0].models.opus = "new-model".into();
    config
}

fn seeded_sessions(provider: &LlmProvider) -> HashMap<String, SessionState> {
    let mut agent_pool = AgentPool::new();
    let model: Arc<dyn peri_model::Model> = Arc::from(provider.clone().into_model());
    agent_pool.store_llm(CachedLlmInstances {
        auxiliary_model: model.clone(),
        auto_classifier_model: Arc::new(tokio::sync::Mutex::new(provider.clone().into_model())),
        fingerprint: "seed-fingerprint".into(),
    });
    agent_pool.subagent_llm_cache.insert("seed".into(), model);
    HashMap::from([(
        "session".into(),
        SessionState {
            session_id: "session".into(),
            thread_id: "session".into(),
            cwd: String::new(),
            environment: None,
            closing: false,
            close_in_flight: 0,
            context_ownership: crate::host::ContextOwnership::Resident,
            history_payloads: Vec::new(),
            structure_version: 0,
            session_generation: crate::host::next_session_generation(),
            history_window: Default::default(),
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
            agent_pool,
            title: None,
            tags: Vec::new(),
        },
    )])
}

async fn assert_persistence_failure_is_atomic(
    config_id: &str,
    failure: PersistenceFailure,
    update: bool,
) {
    let temporary = tempfile::tempdir().unwrap();
    let config = live_config();
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config.clone(), provider.clone(), &temporary).await;
    let revision = cfg.config_source.snapshot().unwrap().revision();
    let mut sessions = seeded_sessions(&provider);
    let cached = sessions["session"]
        .agent_pool
        .get_cached_llm()
        .unwrap()
        .auxiliary_model
        .clone();
    let subagent = sessions["session"].agent_pool.subagent_llm_cache["seed"].clone();
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let path = cfg.config_source.global_path();
    match failure {
        PersistenceFailure::Conflict => {
            std::fs::write(path, r#"{"config":{"language":"external"}}"#).unwrap();
        }
        PersistenceFailure::Io => std::fs::create_dir(path).unwrap(),
    }
    let error = if update {
        let mut candidate = config.clone();
        candidate.config.providers[0].models.sonnet = "updated-model".into();
        handle_update_config(
            &json!({"sessionId":"session", "config":candidate}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err()
    } else {
        let value = if config_id == "model" { "opus" } else { "high" };
        handle_set_config_option(
            &json!({"sessionId":"session", "configId":config_id, "value":value}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err()
    };
    assert_eq!(error.code, -32603);
    assert_eq!(error.message, "Failed to persist config");
    assert_eq!(*cfg.peri_config.read(), config);
    assert_eq!(
        crate::session::agent_pool::fingerprint(&cfg.provider.read()),
        crate::session::agent_pool::fingerprint(&provider)
    );
    let pool = &sessions["session"].agent_pool;
    assert_eq!(pool.fingerprint(), "seed-fingerprint");
    assert!(Arc::ptr_eq(
        &cached,
        &pool.get_cached_llm().unwrap().auxiliary_model
    ));
    assert!(Arc::ptr_eq(&subagent, &pool.subagent_llm_cache["seed"]));
    assert!(mock.notifications().is_empty());
    assert_eq!(cfg.config_source.snapshot().unwrap().revision(), revision);
    match failure {
        PersistenceFailure::Conflict => assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            r#"{"config":{"language":"external"}}"#
        ),
        PersistenceFailure::Io => assert!(path.is_dir()),
    }
}

#[tokio::test]
#[serial]
async fn model_cas_conflict_keeps_live_state_and_cache() {
    assert_persistence_failure_is_atomic("model", PersistenceFailure::Conflict, false).await;
}

#[tokio::test]
#[serial]
async fn effort_cas_conflict_keeps_live_state_and_cache() {
    assert_persistence_failure_is_atomic("thinking_effort", PersistenceFailure::Conflict, false)
        .await;
}

#[tokio::test]
#[serial]
async fn model_io_failure_keeps_live_state_and_cache() {
    assert_persistence_failure_is_atomic("model", PersistenceFailure::Io, false).await;
}

#[tokio::test]
#[serial]
async fn effort_io_failure_keeps_live_state_and_cache() {
    assert_persistence_failure_is_atomic("thinking_effort", PersistenceFailure::Io, false).await;
}

#[tokio::test]
#[serial]
async fn update_config_cas_conflict_keeps_live_state_and_cache() {
    assert_persistence_failure_is_atomic("model", PersistenceFailure::Conflict, true).await;
}

#[tokio::test]
#[serial]
async fn update_config_io_failure_keeps_live_state_and_cache() {
    assert_persistence_failure_is_atomic("model", PersistenceFailure::Io, true).await;
}

#[tokio::test]
#[serial]
async fn successful_options_persist_before_publishing_and_invalidate_cache() {
    for (config_id, value) in [
        ("model", "opus"),
        ("thinking_effort", "high"),
        ("context_1m", "1"),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let config = live_config();
        let provider = LlmProvider::from_config(&config).unwrap();
        let cfg = make_server_config(config.clone(), provider.clone(), &temporary).await;
        let mut sessions = seeded_sessions(&provider);
        let mock = Arc::new(MockTransport::default());
        let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
        handle_set_config_option(
            &json!({"sessionId":"session", "configId":config_id, "value":value}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let persisted = crate::provider::load_from(cfg.config_source.global_path()).unwrap();
        assert_eq!(persisted, *cfg.peri_config.read());
        let resolved = LlmProvider::from_config(&persisted).unwrap();
        assert_eq!(
            crate::session::agent_pool::fingerprint(&cfg.provider.read()),
            crate::session::agent_pool::fingerprint(&resolved)
        );
        assert!(sessions["session"].agent_pool.get_cached_llm().is_none());
        assert!(sessions["session"].agent_pool.subagent_llm_cache.is_empty());
        assert!(!mock.notifications().is_empty());
        match config_id {
            "model" => assert_eq!(cfg.provider.read().model_name(), "new-model"),
            "thinking_effort" => assert_eq!(cfg.provider.read().effort_key(), ":effort=high"),
            "context_1m" => assert!(cfg.provider.read().context_1m()),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
#[serial]
async fn missing_snapshot_rejects_persistence_but_mode_stays_runtime_only() {
    let temporary = tempfile::tempdir().unwrap();
    let config = live_config();
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config.clone(), provider.clone(), &temporary).await;
    let path = cfg.config_source.global_path().to_owned();
    std::fs::write(&path, "{broken}").unwrap();
    cfg.config_source = Arc::new(crate::provider::ConfigSource::load_at_lenient(
        &temporary.path().join("empty-cwd"),
        path.clone(),
    ));
    assert!(cfg.config_source.snapshot().is_none());
    let mut sessions = seeded_sessions(&provider);
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    for config_id in ["model", "thinking_effort", "context_1m"] {
        let error = handle_set_config_option(
            &json!({"sessionId":"session", "configId":config_id, "value":"opus"}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32603);
    }
    let error = handle_update_config(&json!({"config":config}), &cfg, &mut sessions, &transport)
        .await
        .unwrap_err();
    assert_eq!(error.code, -32603);
    assert_eq!(*cfg.peri_config.read(), config);
    assert!(mock.notifications().is_empty());
    assert_eq!(
        sessions["session"].agent_pool.fingerprint(),
        "seed-fingerprint"
    );
    handle_set_config_option(
        &json!({"sessionId":"session", "configId":"mode", "value":"auto"}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(cfg.permission_mode.load(), PermissionMode::AutoMode);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{broken}");
    assert_eq!(*cfg.peri_config.read(), config);
}

#[tokio::test]
#[serial]
async fn update_config_publishes_accepted_settings_and_preserves_owned_fields() {
    let temporary = tempfile::tempdir().unwrap();
    let mut config = live_config();
    config.schema = Some("global-schema".into());
    config.config.extra.insert(
        "mcpServers".into(),
        json!({"owned":{"command":"original-tool"}}),
    );
    config.config.extra.insert("mcpCache".into(), json!(true));
    let global = temporary.path().join("global.json");
    crate::provider::save_to(&config, &global).unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".peri")).unwrap();
    std::fs::write(
        workspace.join(".peri/settings.json"),
        r#"{"$schema":"workspace-schema","config":{"active_alias":"sonnet","mcpCache":false}}"#,
    )
    .unwrap();
    let source = crate::provider::ConfigSource::load_at(&workspace, global).unwrap();
    let initial = source.loaded_merged();
    let provider = LlmProvider::from_config(&initial).unwrap();
    let mut cfg = make_server_config(initial.clone(), provider.clone(), &temporary).await;
    cfg.config_source = Arc::new(source);
    let mut candidate = initial;
    candidate.schema = Some("request-schema".into());
    candidate.config.providers[0].models.sonnet = "accepted-model".into();
    candidate.config.extra.insert(
        "mcpServers".into(),
        json!({"injected":{"command":"request-tool"}}),
    );
    candidate
        .config
        .extra
        .insert("mcpCache".into(), json!(true));
    let mut sessions = seeded_sessions(&provider);
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();

    handle_update_config(
        &json!({"sessionId":"session", "config":candidate}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let accepted = cfg.config_source.snapshot().unwrap();
    assert_eq!(*cfg.peri_config.read(), *accepted.settings());
    let live = cfg.peri_config.read();
    assert_eq!(live.config.extra["mcpCache"], false);
    assert_eq!(
        live.config.extra["mcpServers"],
        config.config.extra["mcpServers"]
    );
    let local: Value = serde_json::from_str(
        &std::fs::read_to_string(cfg.config_source.workspace_path().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(local["$schema"], "workspace-schema");
    assert_eq!(cfg.provider.read().model_name(), "accepted-model");
    assert!(!mock.notifications().is_empty());
}

#[tokio::test]
#[serial]
async fn update_config_rejects_credential_stripped_accepted_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(cwd.join(".peri")).unwrap();
    let workspace_path = cwd.join(".peri/settings.json");
    std::fs::write(&workspace_path, r#"{"config":{"language":"zh-CN"}}"#).unwrap();
    let global_path = temporary.path().join("global.json");
    let mut config = make_peri_config_with_provider(make_provider_config(
        "openai",
        "openai",
        "global-secret",
        "session-model",
    ));
    config.config.providers[0].base_url = "https://old.example/v1".into();
    crate::provider::save_to(&config, &global_path).unwrap();
    let global_before = std::fs::read(&global_path).unwrap();

    let source =
        Arc::new(crate::provider::ConfigSource::load_at(&cwd, global_path.clone()).unwrap());
    let loaded = source.loaded_merged();
    assert_eq!(loaded.config.providers[0].api_key, "global-secret");
    let provider = LlmProvider::from_config(&loaded).unwrap();
    let mut cfg = make_server_config(loaded.clone(), provider.clone(), &temporary).await;
    cfg.config_source = source;
    *cfg.peri_config.write() = loaded.clone();
    *cfg.provider.write() = provider.clone();
    let mut sessions = seeded_sessions(&provider);
    let workspace_before = std::fs::read(&workspace_path).unwrap();
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();

    // 候选仍带全球 key；workspace 保存会因 endpoint 变更剥离它，
    // accepted snapshot 不再有可用 provider。
    let mut candidate = loaded.clone();
    candidate.config.providers[0].base_url = "https://changed.example/v1".into();
    let error = handle_update_config(
        &json!({"config": candidate}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, -32602);
    assert_eq!(
        error.message,
        "accepted configuration has no usable provider"
    );
    assert_eq!(*cfg.peri_config.read(), loaded);
    assert_eq!(
        crate::session::agent_pool::fingerprint(&cfg.provider.read()),
        crate::session::agent_pool::fingerprint(&provider)
    );
    assert_eq!(
        sessions["session"].agent_pool.fingerprint(),
        "seed-fingerprint"
    );
    assert_eq!(std::fs::read(&workspace_path).unwrap(), workspace_before);
    assert_eq!(std::fs::read(&global_path).unwrap(), global_before);
    assert!(!std::fs::read_to_string(&workspace_path)
        .unwrap()
        .contains("global-secret"));
    assert!(mock.notifications().is_empty());

    let reloaded = crate::provider::ConfigSource::load_at(&cwd, global_path)
        .unwrap()
        .loaded_merged();
    assert_eq!(
        reloaded.config.providers[0].base_url,
        "https://old.example/v1"
    );
    assert!(
        LlmProvider::from_config(&reloaded).is_some(),
        "拒绝后重载 provider 不能丢失"
    );
}

#[tokio::test]
#[serial]
async fn update_config_persists_workspace_relative_diff_and_uses_accepted_provider() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path().join("workspace");
    std::fs::create_dir_all(cwd.join(".peri")).unwrap();
    let workspace_path = cwd.join(".peri/settings.json");
    std::fs::write(&workspace_path, r#"{"config":{"language":"zh-CN"}}"#).unwrap();
    let global_path = temporary.path().join("global.json");
    let mut config = make_peri_config_with_provider(make_provider_config(
        "openai",
        "openai",
        "global-secret",
        "old-model",
    ));
    config.config.providers[0].base_url = "https://old.example/v1".into();
    // 工作区文档缺省 active_alias 时按 opus 生效。
    config.config.providers[0].models.opus = "old-model".into();
    crate::provider::save_to(&config, &global_path).unwrap();

    let source =
        Arc::new(crate::provider::ConfigSource::load_at(&cwd, global_path.clone()).unwrap());
    let loaded = source.loaded_merged();
    assert_eq!(loaded.config.providers[0].models.opus, "old-model");
    let provider = LlmProvider::from_config(&loaded).unwrap();
    let mut cfg = make_server_config(loaded.clone(), provider.clone(), &temporary).await;
    cfg.config_source = source;
    *cfg.peri_config.write() = loaded.clone();
    *cfg.provider.write() = provider;
    let mut sessions = seeded_sessions(&LlmProvider::from_config(&loaded).unwrap());
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();

    let mut candidate = loaded.clone();
    candidate.config.providers[0].models.opus = "updated-model".into();
    handle_update_config(
        &json!({"sessionId":"session", "config": candidate}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let workspace_text = std::fs::read_to_string(&workspace_path).unwrap();
    assert!(
        !workspace_text.contains("global-secret"),
        "工作区保存不得复制全局 key"
    );
    let workspace: Value = serde_json::from_str(&workspace_text).unwrap();
    assert_eq!(workspace["config"]["providers"][0]["apiKey"], "");
    let accepted = crate::provider::ConfigSource::load_at(&cwd, global_path)
        .unwrap()
        .loaded_merged();
    assert_eq!(*cfg.peri_config.read(), accepted);
    let accepted_provider = LlmProvider::from_config(&accepted).unwrap();
    assert_eq!(cfg.provider.read().model_name(), "updated-model");
    assert_eq!(
        crate::session::agent_pool::fingerprint(&cfg.provider.read()),
        crate::session::agent_pool::fingerprint(&accepted_provider),
        "运行态 provider 必须来自 accepted 配置"
    );
    assert!(!mock.notifications().is_empty());
}

#[tokio::test]
#[serial]
async fn invalid_model_candidate_is_rejected_before_persistence() {
    let temporary = tempfile::tempdir().unwrap();
    let config = live_config();
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config.clone(), provider.clone(), &temporary).await;
    let mut sessions = seeded_sessions(&provider);
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let revision = cfg.config_source.snapshot().unwrap().revision();
    let error = handle_set_config_option(
        &json!({"sessionId":"session", "configId":"model", "value":"unknown-tier"}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(*cfg.peri_config.read(), config);
    assert_eq!(cfg.provider.read().model_name(), "old-model");
    assert_eq!(cfg.config_source.snapshot().unwrap().revision(), revision);
    assert!(!cfg.config_source.global_path().exists());
    assert_eq!(
        sessions["session"].agent_pool.fingerprint(),
        "seed-fingerprint"
    );
    assert!(mock.notifications().is_empty());
}
