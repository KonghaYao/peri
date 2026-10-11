use super::*;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::PeriCaps;

async fn binding_state(cfg: &AcpServerConfig, id: &str) -> BindingState {
    cfg.session_resources
        .load_session_binding(&id.to_owned())
        .await
        .unwrap()
}

async fn frozen_state(cfg: &AcpServerConfig, id: &str) -> FrozenState {
    cfg.session_resources
        .load_session_snapshot(&id.to_owned())
        .await
        .unwrap()
        .frozen
}

struct Fixture {
    cfg: AcpServerConfig,
    bridge: Arc<SqliteThreadStore>,
    tmp: tempfile::TempDir,
    cwd: PathBuf,
    id: String,
    sessions: HashMap<String, SessionState>,
    transport: Arc<dyn crate::transport::AcpTransport>,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(tmp.path()).unwrap().join("saved");
        std::fs::create_dir(&cwd).unwrap();
        let config = make_peri_config_with_provider(make_provider_config(
            "local",
            "openai",
            "fixture-key",
            "fixture-model",
        ));
        let (cfg, bridge) = make_server_config_with_bridge(
            config.clone(),
            LlmProvider::from_config(&config).unwrap(),
            &tmp,
        )
        .await;
        let mut sessions = HashMap::new();
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let created = handle_request(
            "session/new",
            &json!({"cwd":cwd}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let id = created["sessionId"].as_str().unwrap().to_owned();
        bridge
            .append_message(&id, BaseMessage::human("preserved session history"))
            .await
            .unwrap();
        Self {
            cfg,
            bridge,
            tmp,
            cwd,
            id,
            sessions,
            transport,
        }
    }

    async fn request(&mut self, method: &str, params: &Value) -> Result<Value, AcpError> {
        handle_request(
            method,
            params,
            &self.cfg,
            &mut self.sessions,
            &self.transport,
        )
        .await
    }

    async fn close(&mut self, id: &str) {
        self.request("session/close", &json!({"sessionId":id}))
            .await
            .unwrap();
    }

    fn assert_restored_history(&self, id: &str) {
        let state = &self.sessions[id];
        assert_eq!(Path::new(&state.cwd), self.cwd);
        assert_eq!(state.history[0].content(), "preserved session history");
        assert!(state.frozen.is_some());
    }
}

#[tokio::test]
#[serial]
async fn test_clean_load_resume_and_fork_ignore_caller_cwd_and_keep_saved_facts() {
    for caps in [PeriCaps::default(), PeriCaps::all_enabled()] {
        for method in ["session/load", "session/resume", "session/fork"] {
            let mut fixture = Fixture::new().await;
            let binding = binding_state(&fixture.cfg, &fixture.id).await;
            let frozen = frozen_state(&fixture.cfg, &fixture.id).await;
            let id = fixture.id.clone();
            fixture.close(&id).await;
            fixture.cfg.session_manager.set_pending_caps(caps.clone());
            let other = tempfile::tempdir().unwrap();
            let params = json!({"sessionId":fixture.id,"cwd":other.path()});
            let response = fixture.request(method, &params).await.unwrap();
            assert!(response
                .pointer("/_meta/peri.sessionWorkspaceV1/read_only")
                .is_none());
            fixture.assert_restored_history(&fixture.id);
            assert_eq!(binding_state(&fixture.cfg, &fixture.id).await, binding);
            assert_eq!(frozen_state(&fixture.cfg, &fixture.id).await, frozen);
            assert_eq!(
                Path::new(&fixture.bridge.load_meta(&fixture.id).await.unwrap().cwd),
                fixture.cwd
            );
            if method == "session/fork" {
                let fork_id = response["sessionId"].as_str().unwrap();
                assert_ne!(fork_id, fixture.id);
                fixture.assert_restored_history(fork_id);
                assert_eq!(frozen_state(&fixture.cfg, fork_id).await, frozen);
                fixture.close(fork_id).await;
            }
            let id = fixture.id.clone();
            fixture.close(&id).await;
        }
    }
}

#[tokio::test]
#[serial]
async fn fork_rejects_current_prompt_without_persistent_execution_state() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    fixture.sessions.get_mut(&id).unwrap().cancel_token =
        Some(tokio_util::sync::CancellationToken::new());
    let error = fixture
        .request("session/fork", &json!({"sessionId": id}))
        .await
        .unwrap_err();
    assert!(error.message.contains("execution is active"));
    assert_eq!(fixture.sessions.len(), 1);
    fixture.sessions.get_mut(&id).unwrap().cancel_token = None;
    fixture.close(&id).await;
}

#[tokio::test]
#[serial]
async fn load_and_resume_after_runtime_loss_preserve_session_history() {
    for method in ["session/load", "session/resume"] {
        let mut fixture = Fixture::new().await;
        let id = fixture.id.clone();
        let binding = binding_state(&fixture.cfg, &id).await;
        let frozen = frozen_state(&fixture.cfg, &id).await;
        fixture.sessions.clear();
        let response = fixture
            .request(method, &json!({"sessionId":id}))
            .await
            .unwrap();
        fixture.assert_restored_history(&id);
        assert!(response
            .pointer("/_meta/peri.sessionWorkspaceV1/read_only")
            .is_none());
        assert!(response
            .pointer("/_meta/peri.sessionWorkspaceV1/restore_warning")
            .is_none());
        assert_eq!(binding_state(&fixture.cfg, &id).await, binding);
        assert_eq!(frozen_state(&fixture.cfg, &id).await, frozen);
        fixture
            .request(
                "session/rename",
                &json!({"sessionId":id,"title":"restored"}),
            )
            .await
            .unwrap();
        assert_eq!(
            fixture
                .bridge
                .load_meta(&id)
                .await
                .unwrap()
                .title
                .as_deref(),
            Some("restored")
        );
        fixture.close(&id).await;
    }
}

#[tokio::test]
#[serial]
async fn another_host_handle_can_restore_without_peri_execution_exclusion() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    let configuration = fixture.cfg.peri_config.read().clone();
    let other = make_server_config(
        configuration.clone(),
        LlmProvider::from_config(&configuration).unwrap(),
        &fixture.tmp,
    )
    .await;
    let mut other_sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":id}),
        &other,
        &mut other_sessions,
        &fixture.transport,
    )
    .await
    .unwrap();
    assert!(other_sessions[&id].frozen.is_some());
    assert_eq!(
        other_sessions[&id].history[0].content(),
        "preserved session history"
    );
    assert!(fixture.sessions.contains_key(&id));
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &other,
        &mut other_sessions,
        &fixture.transport,
    )
    .await
    .unwrap();
    fixture.close(&id).await;
}

#[tokio::test]
#[serial]
async fn unloaded_close_and_delete_settle_normally() {
    for delete in [false, true] {
        let mut fixture = Fixture::new().await;
        let id = fixture.id.clone();
        fixture.sessions.clear();
        let method = if delete {
            "session/delete"
        } else {
            "session/close"
        };
        fixture
            .request(method, &json!({"sessionId":id}))
            .await
            .unwrap();
        fixture
            .request(method, &json!({"sessionId":id}))
            .await
            .unwrap();
        assert!(fixture.sessions.is_empty());
        if delete {
            assert!(fixture.bridge.load_meta(&id).await.is_err());
        } else {
            assert!(!fixture
                .cfg
                .session_resources
                .is_session_closing(&id)
                .await
                .unwrap());
            assert_eq!(
                fixture
                    .cfg
                    .session_resources
                    .close_settlement(&id)
                    .await
                    .unwrap(),
                peri_acp_types::session_resources::CloseSettlement::Finished
            );
        }
    }
}

#[tokio::test]
#[serial]
async fn persisted_closing_intent_blocks_restore_until_close_drains() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    fixture
        .cfg
        .session_resources
        .mark_session_closing(&id)
        .await
        .unwrap();
    fixture.sessions.clear();
    for method in ["session/load", "session/resume", "session/fork"] {
        let error = fixture
            .request(method, &json!({"sessionId":id}))
            .await
            .unwrap_err();
        assert!(error.message.contains("close incomplete"));
        assert!(fixture.sessions.is_empty());
    }
    fixture.close(&id).await;
    fixture
        .request("session/load", &json!({"sessionId":id}))
        .await
        .unwrap();
    fixture.assert_restored_history(&id);
    fixture.close(&id).await;
}

#[tokio::test]
#[serial]
async fn dirty_reset_rpc_stays_removed_and_does_not_change_session_facts() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    let binding = binding_state(&fixture.cfg, &id).await;
    let frozen = frozen_state(&fixture.cfg, &id).await;
    let error = fixture
        .request(
            "peri/session_reset_dirty",
            &json!({"target":{"thread_id":id,"generation":1},"accept_risk":true}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, -32601);
    assert_eq!(binding_state(&fixture.cfg, &id).await, binding);
    assert_eq!(frozen_state(&fixture.cfg, &id).await, frozen);
    fixture.close(&id).await;
}

#[tokio::test]
#[serial]
async fn read_only_store_keeps_history_readable_but_rejects_load_and_close_mutations() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    fixture.close(&id).await;
    let config = fixture.cfg.peri_config.read().clone();
    let resources = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(
            fixture.tmp.path().join("threads.db"),
        )
        .await
        .unwrap(),
    );
    let readonly = build_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &fixture.tmp,
        resources,
    )
    .await;
    let mut sessions = HashMap::new();
    let history = handle_request(
        "peri/session_history",
        &json!({"sessionId":id}),
        &readonly,
        &mut sessions,
        &fixture.transport,
    )
    .await
    .unwrap();
    assert_eq!(history["payloads"].as_array().unwrap().len(), 1);
    for method in [
        "session/load",
        "session/resume",
        "session/close",
        "session/delete",
    ] {
        assert!(handle_request(
            method,
            &json!({"sessionId":id}),
            &readonly,
            &mut sessions,
            &fixture.transport
        )
        .await
        .is_err());
        assert!(sessions.is_empty());
    }
    assert!(!fixture
        .cfg
        .session_resources
        .is_session_closing(&id)
        .await
        .unwrap());
    assert!(fixture.bridge.load_meta(&id).await.is_ok());
}
