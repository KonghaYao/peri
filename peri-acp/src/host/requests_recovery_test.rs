use super::*;
use peri_acp_types::PeriCaps;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::workspace::ReadOnlyAdmission;

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

fn read_only(response: &Value) -> Option<ReadOnlyAdmission> {
    response
        .pointer("/_meta/peri.sessionWorkspaceV1/read_only")
        .map(|value| serde_json::from_value(value.clone()).unwrap())
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
            .append_message(&id, BaseMessage::human("preserved recovery history"))
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

    fn assert_owned_history(&self, id: &str) {
        let state = &self.sessions[id];
        assert_eq!(Path::new(&state.cwd), self.cwd);
        assert_eq!(state.history[0].content(), "preserved recovery history");
        assert!(state.execution_owner.is_some());
        assert!(state.frozen.is_some());
    }

    fn assert_read_only_history(&self, id: &str) {
        let state = &self.sessions[id];
        assert_eq!(Path::new(&state.cwd), self.cwd);
        assert_eq!(state.history[0].content(), "preserved recovery history");
        assert!(state.execution_owner.is_none());
        assert!(state.environment.is_none());
        assert!(state.frozen.is_none());
        assert!(state.workflow_middleware.is_none());
    }
}

#[tokio::test]
#[serial]
async fn test_unloaded_close_preserves_intent_until_former_owner_is_fenced() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    fixture
        .cfg
        .session_resources
        .mark_session_closing(&id)
        .await
        .unwrap();
    fixture.sessions.clear();
    let error = fixture
        .request("session/close", &json!({"sessionId": id}))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32010);
    assert!(
        fixture
            .cfg
            .session_resources
            .is_session_closing(&id)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[serial]
async fn test_unloaded_close_cannot_initiate_close_of_active_session() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    fixture.sessions.clear();
    let error = fixture
        .request("session/close", &json!({"sessionId": id}))
        .await
        .unwrap_err();
    assert!(error.message.contains("execution owner"));
    assert!(
        !fixture
            .cfg
            .session_resources
            .is_session_closing(&id)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[serial]
async fn test_dirty_load_resume_and_fork_ignore_caller_cwd_and_keep_saved_facts() {
    for caps in [PeriCaps::default(), PeriCaps::all_enabled()] {
        for method in ["session/load", "session/resume", "session/fork"] {
            let mut fixture = Fixture::new().await;
            let binding = binding_state(&fixture.cfg, &fixture.id).await;
            let frozen = frozen_state(&fixture.cfg, &fixture.id).await;
            fixture.sessions.clear();
            fixture.cfg.session_manager.set_pending_caps(caps.clone());
            let other = tempfile::tempdir().unwrap();
            let params = json!({"sessionId":fixture.id,"cwd":other.path()});
            let response = fixture.request(method, &params).await.unwrap();
            assert!(read_only(&response).is_none());
            fixture.assert_owned_history(&fixture.id);
            assert_eq!(binding_state(&fixture.cfg, &fixture.id).await, binding);
            assert_eq!(frozen_state(&fixture.cfg, &fixture.id).await, frozen);
            assert_eq!(
                Path::new(&fixture.bridge.load_meta(&fixture.id).await.unwrap().cwd),
                fixture.cwd
            );
            if method == "session/fork" {
                let fork_id = response["sessionId"].as_str().unwrap();
                assert_ne!(fork_id, fixture.id);
                fixture.assert_owned_history(fork_id);
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
async fn test_load_with_another_instance_handle_rejects_a_second_execution_owner() {
    let mut fixture = Fixture::new().await;
    let original_owner = fixture.sessions[&fixture.id]
        .execution_owner
        .clone()
        .unwrap();
    let binding = binding_state(&fixture.cfg, &fixture.id).await;
    let frozen = frozen_state(&fixture.cfg, &fixture.id).await;
    let config = fixture.cfg.peri_config.read().clone();
    let other_cfg = make_server_config(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &fixture.tmp,
    )
    .await;
    other_cfg
        .session_manager
        .set_pending_caps(PeriCaps::all_enabled());
    let mut other_sessions = HashMap::new();
    let other_cwd = tempfile::tempdir().unwrap();
    let error = handle_request(
        "session/load",
        &json!({"sessionId":fixture.id,"cwd":other_cwd.path()}),
        &other_cfg,
        &mut other_sessions,
        &fixture.transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32010);
    assert!(error.message.contains("execution owner is still active"));
    assert!(!other_sessions.contains_key(&fixture.id));
    assert!(Arc::ptr_eq(
        fixture.sessions[&fixture.id]
            .execution_owner
            .as_ref()
            .unwrap(),
        &original_owner,
    ));
    fixture
        .request(
            "session/rename",
            &json!({"sessionId":fixture.id,"title":"original instance"}),
        )
        .await
        .unwrap();
    assert_eq!(binding_state(&fixture.cfg, &fixture.id).await, binding);
    assert_eq!(frozen_state(&fixture.cfg, &fixture.id).await, frozen);
    let id = fixture.id.clone();
    fixture.close(&id).await;
}

#[tokio::test]
#[serial]
async fn test_missing_saved_directory_loads_history_without_tools_then_can_upgrade() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    let binding = binding_state(&fixture.cfg, &id).await;
    let frozen = frozen_state(&fixture.cfg, &id).await;
    fixture.close(&id).await;
    std::fs::remove_dir(&fixture.cwd).unwrap();
    fixture
        .cfg
        .session_manager
        .set_pending_caps(PeriCaps::all_enabled());
    for method in ["session/load", "session/resume"] {
        let response = fixture
            .request(method, &json!({"sessionId":id,"cwd":fixture.tmp.path()}))
            .await
            .unwrap();
        assert_eq!(
            read_only(&response),
            Some(ReadOnlyAdmission::ExecutionLeaseRequired)
        );
        fixture.assert_read_only_history(&id);
        assert_eq!(binding_state(&fixture.cfg, &id).await, binding);
        assert_eq!(frozen_state(&fixture.cfg, &id).await, frozen);
    }
    let error = fixture
        .request(
            "session/rename",
            &json!({"sessionId":id,"title":"must not write"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, -32010);
    assert_ne!(
        fixture
            .bridge
            .load_meta(&id)
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("must not write")
    );
    std::fs::create_dir(&fixture.cwd).unwrap();
    let error = fixture
        .request("session/load", &json!({"sessionId":id}))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32010);
    fixture.assert_read_only_history(&id);
    assert_eq!(binding_state(&fixture.cfg, &id).await, binding);
    assert_eq!(frozen_state(&fixture.cfg, &id).await, frozen);
    fixture.close(&id).await;
}

#[tokio::test]
#[serial]
async fn test_recovery_capability_is_false_and_reset_dirty_rpc_is_removed() {
    let mut fixture = Fixture::new().await;
    let id = fixture.id.clone();
    fixture.sessions.clear();
    let binding = binding_state(&fixture.cfg, &id).await;
    let frozen = frozen_state(&fixture.cfg, &id).await;
    let response = fixture
        .request(
            "initialize",
            &json!({
                "protocolVersion":1,
                "clientCapabilities":{"_meta":{
                    "peri.sessionRecoveryV1":true,"peri.sessionWorkspaceV1":true
                }}
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        response["agentCapabilities"]["_meta"]["peri.sessionRecoveryV1"],
        false
    );
    assert!(!PeriCaps::all_enabled().session_recovery_v1);
    assert!(
        !PeriCaps::from_client_meta(json!({"peri.sessionRecoveryV1":true}).as_object().unwrap())
            .session_recovery_v1
    );
    let error = fixture
        .request(
            "peri/session_reset_dirty",
            &json!({
                "target":{"thread_id":id,"generation":1},"accept_risk":true
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, -32601);
    assert!(fixture.sessions.is_empty());
    assert_eq!(binding_state(&fixture.cfg, &id).await, binding);
    assert_eq!(frozen_state(&fixture.cfg, &id).await, frozen);
    fixture
        .request("session/load", &json!({"sessionId":id}))
        .await
        .unwrap();
    fixture.assert_owned_history(&id);
    fixture.close(&id).await;
}

#[tokio::test]
async fn foreign_machine_history_fixture() {
    let Ok(directory) = std::env::var("PERI_ACP_FOREIGN_HISTORY_FIXTURE") else {
        return;
    };
    let root = Path::new(&directory);
    let store = SqliteThreadStore::new(root.join("threads.db"))
        .await
        .unwrap();
    let id = store
        .create_thread(ThreadMeta::new(root.join("saved").to_str().unwrap()))
        .await
        .unwrap();
    store
        .append_message(&id, BaseMessage::human("foreign machine history"))
        .await
        .unwrap();
    store
        .store_frozen_snapshot_if_absent(&id, "broken foreign frozen")
        .await
        .unwrap();
    std::fs::write(root.join("foreign-session-id"), id).unwrap();
}

#[tokio::test]
#[serial]
async fn test_foreign_machine_history_is_read_only_without_legacy_adoption_or_tools() {
    let mut fixture = Fixture::new().await;
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "host::requests::tests::recovery_tests::foreign_machine_history_fixture",
        ])
        .env("PERI_MACHINE_ID", uuid::Uuid::new_v4().to_string())
        .env(
            "PERI_ACP_FOREIGN_HISTORY_FIXTURE",
            std::fs::canonicalize(fixture.tmp.path()).unwrap(),
        )
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = std::fs::read_to_string(fixture.tmp.path().join("foreign-session-id")).unwrap();
    fixture
        .cfg
        .session_manager
        .set_pending_caps(PeriCaps::all_enabled());
    let binding = binding_state(&fixture.cfg, &id).await;
    let frozen = frozen_state(&fixture.cfg, &id).await;
    assert!(
        fixture
            .bridge
            .load_session_binding(&id)
            .await
            .unwrap()
            .is_none()
    );
    for method in ["session/load", "session/resume"] {
        let response = fixture
            .request(method, &json!({"sessionId":id}))
            .await
            .unwrap();
        assert_eq!(
            read_only(&response),
            Some(ReadOnlyAdmission::ExecutionLeaseRequired)
        );
        let state = &fixture.sessions[&id];
        assert_eq!(Path::new(&state.cwd), fixture.cwd);
        assert_eq!(state.history[0].content(), "foreign machine history");
        assert!(state.execution_owner.is_none());
        assert!(state.environment.is_none());
        assert!(state.frozen.is_none());
        assert!(state.workflow_middleware.is_none());
        assert_eq!(binding_state(&fixture.cfg, &id).await, binding);
        assert_eq!(frozen_state(&fixture.cfg, &id).await, frozen);
        assert!(
            fixture
                .bridge
                .load_session_binding(&id)
                .await
                .unwrap()
                .is_none()
        );
    }
    fixture.close(&id).await;
    let original_id = fixture.id.clone();
    fixture.close(&original_id).await;
}
