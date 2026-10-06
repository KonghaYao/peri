use super::*;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::session::TurnId;
use peri_acp_types::session_resources::ControlAttempt;
use serde_json::json;

fn admission() -> WorkAdmission {
    WorkAdmission {
        session_id: "session".into(),
        admission_id: "admission".into(),
        instance_id: "sdk-instance".into(),
        generation_id: "sdk-generation".into(),
        lifecycle: 1,
        control_generation: 2,
        work_id: "work".into(),
        work_revision: 0,
        execution: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    }
}

#[test]
fn default_mode_cannot_fall_back_without_an_admission() {
    assert!(matches!(
        WorkRuntime::bind(WorkMode::default(), None, None),
        Err(WorkSetupError::MissingAdmission)
    ));
}

#[tokio::test]
async fn bound_store_without_sdk_ticket_cannot_bypass_required_work() {
    let session = crate::session::test_resources::TestSession::open().await;
    assert!(matches!(
        WorkRuntime::bind(WorkMode::Required, None, Some(session.resources)),
        Err(WorkSetupError::MissingAdmission)
    ));
}

#[test]
fn required_ticket_without_resources_cannot_start_a_best_effort_pipeline() {
    assert!(matches!(
        WorkRuntime::bind(WorkMode::Required, Some(&admission()), None),
        Err(WorkSetupError::MissingResources)
    ));
}

#[test]
fn best_effort_fixture_requires_explicit_test_only_opt_in() {
    assert!(matches!(
        WorkRuntime::bind(WorkMode::BestEffortFixture, None, None).unwrap(),
        WorkRuntime::BestEffortFixture
    ));
}

#[test]
fn checkpoint_retains_full_request_and_digests_its_exact_bytes() {
    let request = json!({"messages":[{"role":"user","content":"input"}],
        "tools":[{"name":"tool","schema":{"type":"object"}}],"config":{"temperature":0}});
    let checkpoint = request_checkpoint(&request, "model".into(), "authorization".into()).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&checkpoint.serialized_request).unwrap(),
        request
    );
    assert_eq!(
        checkpoint.request_digest,
        format!(
            "{:x}",
            Sha256::digest(checkpoint.serialized_request.as_bytes())
        )
    );
    assert_eq!(checkpoint.model_ref, "model");
    assert_eq!(checkpoint.authorization_ref, "authorization");
}
