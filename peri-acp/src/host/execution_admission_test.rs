use super::*;
use crate::transport::types::AcpError;
use peri_acp_types::{
    execution_admission::AttemptStoppedProof,
    session_resources::{
        work::{WorkAdmission, WorkCandidate, WorkStage},
        ControlState,
    },
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

struct FixtureTransport {
    result: Result<Value, AcpError>,
    calls: AtomicUsize,
}

#[async_trait]
impl RequestTransport for FixtureTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

fn fixture(result: Result<Value, AcpError>) -> (Arc<FixtureTransport>, ReverseExecutionAdmission) {
    let transport = Arc::new(FixtureTransport {
        result,
        calls: AtomicUsize::new(0),
    });
    let port = ReverseExecutionAdmission::new(transport.clone());
    (transport, port)
}

fn admission() -> WorkAdmission {
    serde_json::from_value(json!({
        "sessionId":"s", "admissionId":"ticket", "instanceId":"i", "generationId":"g",
        "lifecycle":1, "controlGeneration":0, "workId":"w", "workRevision":0,
        "execution":{"turnId":"00000000-0000-4000-8000-000000000001","attemptId":"a"},
    }))
    .unwrap()
}

fn request() -> AdmissionRequest {
    AdmissionRequest {
        existing_admission: None,
        request_id: "stable-request".into(),
        snapshot: peri_acp_types::execution_admission::AdmissionSnapshot {
            session_id: "s".into(),
            control: ControlState::default(),
            blocked: false,
            candidates: vec![WorkCandidate {
                work_id: "w".into(),
                work_revision: 0,
                stage: WorkStage::ReasonReady,
                delivery_ids: Vec::new(),
                requires_recovery: false,
            }],
        },
    }
}

fn settlement() -> SettlementRequest {
    let admission = admission();
    SettlementRequest {
        proof: AttemptStoppedProof::AttemptStopped {
            instance_id: admission.instance_id.clone(),
            generation_id: admission.generation_id.clone(),
            execution: admission.execution.clone(),
            evidence_id: "actual-attempt-stopped".into(),
        },
        admission,
    }
}

#[tokio::test]
async fn reverse_fixture_preserves_exact_ticket() {
    let (_, port) = fixture(Ok(json!({"status":"admitted","admission":admission()})));
    assert_eq!(
        port.admit(request()).await.unwrap(),
        AdmissionOutcome::Admitted {
            admission: admission()
        }
    );
}

#[tokio::test]
async fn reverse_fixture_rejects_wrong_work_association() {
    let mut wrong = admission();
    wrong.work_revision += 1;
    let (_, port) = fixture(Ok(json!({"status":"admitted","admission":wrong})));
    assert!(matches!(
        port.admit(request()).await,
        Err(ExecutionAdmissionError::Protocol(_))
    ));
}

#[tokio::test]
async fn reverse_fixture_existing_confirmation_cannot_mint_another_ticket() {
    let mut changed = admission();
    changed.admission_id = "new-ticket".into();
    let (_, port) = fixture(Ok(json!({"status":"admitted","admission":changed})));
    let mut request = request();
    request.existing_admission = Some(admission());
    assert!(matches!(
        port.admit(request).await,
        Err(ExecutionAdmissionError::Protocol(_))
    ));
}

#[tokio::test]
async fn reverse_fixture_missing_capability_has_no_fallback() {
    let (_, port) = fixture(Err(AcpError::new(-32601, "unsupported")));
    assert!(matches!(
        port.admit(request()).await,
        Err(ExecutionAdmissionError::Unavailable)
    ));
}

#[tokio::test]
async fn reverse_fixture_disconnect_preserves_unknown_ownership() {
    let (_, port) = fixture(Err(AcpError::new(-32603, "disconnected after commit")));
    assert_eq!(
        port.admit(request()).await.unwrap(),
        AdmissionOutcome::Unknown
    );
    assert_eq!(
        port.settle(settlement()).await.unwrap(),
        SettlementOutcome::Unknown
    );
}

#[tokio::test]
async fn reverse_fixture_cannot_settle_wrong_attempt() {
    let (transport, port) = fixture(Ok(json!({"status":"unknown"})));
    let mut request = settlement();
    request.admission.generation_id = "different".into();
    assert!(matches!(
        port.settle(request).await,
        Err(ExecutionAdmissionError::Protocol(_))
    ));
    assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn reverse_fixture_receipt_requires_exact_stopped_evidence() {
    let (_, port) = fixture(Ok(json!({"status":"applied","receipt":{
        "admission":admission(),"evidenceId":"other-attempt",
    }})));
    assert!(matches!(
        port.settle(settlement()).await,
        Err(ExecutionAdmissionError::Protocol(_))
    ));
}

#[tokio::test]
async fn reverse_fixture_entered_requires_exact_durable_entry_receipt() {
    let (_, port) = fixture(Ok(json!({"status":"applied","receipt":{
        "admission":admission(),"entryEvidenceId":"other-entry",
    }})));
    assert!(matches!(
        port.entered(EntryRequest {
            admission: admission(),
            entry_evidence_id: "durable-entry".into()
        })
        .await,
        Err(ExecutionAdmissionError::Protocol(_))
    ));
    let (_, port) = fixture(Err(AcpError::new(-32603, "lost entry ACK")));
    assert_eq!(
        port.entered(EntryRequest {
            admission: admission(),
            entry_evidence_id: "durable-entry".into()
        })
        .await
        .unwrap(),
        EntryOutcome::Unknown
    );
}
