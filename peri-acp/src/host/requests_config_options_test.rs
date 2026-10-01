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
            execution_owner: None,
            environment: None,
            closing: false,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool,
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
