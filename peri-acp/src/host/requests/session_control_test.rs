use super::*;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::session::TurnId;
use peri_acp_types::session_resources::{
    ControlAction, ControlAttempt, ControlCommand, ControlStatus,
};
use tokio_util::sync::CancellationToken;

#[path = "session_control/reopen_test.rs"]
mod reopen_tests;

async fn fixture(
    tmp: &tempfile::TempDir,
) -> (AcpServerConfig, HashMap<String, SessionState>, String) {
    let config = make_peri_config_with_provider(make_provider_config(
        "test",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = make_server_config(config, provider, tmp).await;
    let mut sessions = HashMap::new();
    let id = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager
        .ensure_session(&id, tmp.path().to_str().unwrap());
    (cfg, sessions, id)
}

async fn command(
    cfg: &AcpServerConfig,
    id: &str,
    command_id: &str,
    action: ControlAction,
) -> Value {
    let state = cfg
        .session_resources
        .load_session_control(&id.to_owned())
        .await
        .unwrap();
    serde_json::to_value(ControlCommand {
        session_id: id.to_owned(),
        command_id: command_id.to_owned(),
        expected_lifecycle: state.lifecycle,
        expected_revision: state.revision,
        expected_control_generation: state.control_generation,
        action,
    })
    .unwrap()
}

#[tokio::test]
async fn pause_replay_returns_original_receipt_without_cancelling_new_attempt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, id) = fixture(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let original = CancellationToken::new();
    sessions.get_mut(&id).unwrap().cancel_token = Some(original.clone());
    let pause = command(&cfg, &id, "pause-1", ControlAction::Pause).await;
    let receipt = handle_request("session/control", &pause, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(original.is_cancelled());
    let resume = command(&cfg, &id, "resume-1", ControlAction::Resume).await;
    handle_request("session/control", &resume, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    let replacement = CancellationToken::new();
    sessions.get_mut(&id).unwrap().cancel_token = Some(replacement.clone());
    assert_eq!(
        handle_request("session/control", &pause, &cfg, &mut sessions, &transport)
            .await
            .unwrap(),
        receipt
    );
    assert!(!replacement.is_cancelled());
    let snapshot = handle_request(
        "session/control/state",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(snapshot["state"]["status"], "active");
}

#[tokio::test]
async fn stop_rejects_wrong_execution_and_matches_exact_current_attempt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, id) = fixture(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let target = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let state = cfg
        .session_resources
        .load_session_control(&id)
        .await
        .unwrap();
    cfg.session_resources
        .apply_session_control(&ControlCommand {
            session_id: id.clone(),
            command_id: "observe-1".into(),
            expected_lifecycle: state.lifecycle,
            expected_revision: state.revision,
            expected_control_generation: state.control_generation,
            action: ControlAction::ObserveAttempt {
                target: Some(target.clone()),
            },
        })
        .await
        .unwrap();
    let token = CancellationToken::new();
    sessions.get_mut(&id).unwrap().cancel_token = Some(token.clone());
    let wrong = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let stale = command(
        &cfg,
        &id,
        "stop-stale",
        ControlAction::Stop { target: wrong },
    )
    .await;
    let rejected = handle_request("session/control", &stale, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(rejected["decision"]["kind"], "rejected");
    assert!(!token.is_cancelled());
    let stop = command(&cfg, &id, "stop-current", ControlAction::Stop { target }).await;
    let receipt = handle_request("session/control", &stop, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(receipt["state"]["status"], "paused");
    assert!(token.is_cancelled());

    assert_eq!(
        cfg.session_resources
            .load_session_control(&id)
            .await
            .unwrap()
            .status,
        peri_acp_types::session_resources::ControlStatus::Paused
    );
}

#[tokio::test]
async fn independent_child_prevents_false_close_without_being_cancelled() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, mut sessions, id) = fixture(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let runtime = peri_acp_types::session::AgentRuntime::new(
        "child".to_owned(),
        peri_acp_types::thread::CancelPolicy::Independent,
    );
    let child_token = runtime.cancel_token.clone();
    cfg.session_manager
        .get_session_mut(&id)
        .unwrap()
        .active_agents
        .insert("child".to_owned(), runtime);
    let close = command(&cfg, &id, "close-1", ControlAction::Close).await;
    let receipt = handle_request("session/control", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(receipt["state"]["status"], "closing");
    assert!(!child_token.is_cancelled());
    assert!(sessions.contains_key(&id));
    assert_eq!(
        cfg.session_resources
            .load_session_control(&id)
            .await
            .unwrap()
            .status,
        ControlStatus::Closing
    );
    let snapshot = handle_request(
        "session/control/state",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_ne!(snapshot["settlement"]["status"], "settled");
}
