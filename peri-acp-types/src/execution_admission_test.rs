use super::*;
use serde_json::json;

#[test]
fn proof_wire_is_exact_sdk_attempt_association() {
    let proof: AttemptStoppedProof = serde_json::from_value(json!({
        "kind":"attemptStopped", "instanceId":"i", "generationId":"g",
        "execution":{"turnId":"00000000-0000-4000-8000-000000000001", "attemptId":"a"}, "evidenceId":"stopped",
    })).unwrap();
    let mut admission: WorkAdmission = serde_json::from_value(json!({
        "sessionId":"s", "admissionId":"ticket", "instanceId":"i", "generationId":"g",
        "lifecycle":1, "controlGeneration":0, "workId":"w", "workRevision":0,
        "execution":{"turnId":"00000000-0000-4000-8000-000000000001", "attemptId":"a"},
    }))
    .unwrap();
    assert!(proof.validates(&admission));
    admission.generation_id = "other-generation".into();
    assert!(!proof.validates(&admission));
    assert_eq!(
        serde_json::to_value(&proof).unwrap()["kind"],
        "attemptStopped"
    );
}

#[test]
fn unknown_is_never_an_applied_receipt() {
    assert_eq!(
        serde_json::from_value::<SettlementOutcome>(json!({"status":"unknown"})).unwrap(),
        SettlementOutcome::Unknown
    );
    assert!(serde_json::from_value::<SettlementOutcome>(json!({"status":"applied"})).is_err());
    assert_eq!(
        serde_json::from_value::<SettlementOutcome>(json!({"status":"unknown", "receipt":{}}))
            .unwrap(),
        SettlementOutcome::Unknown
    );
    assert!(serde_json::from_value::<AttemptStoppedProof>(
        json!({"kind":"instanceStopped", "instanceId":"i", "generationId":"g", "evidenceId":"e"})
    )
    .is_err());
}

#[test]
fn receipt_preserves_ticket_and_execution_fields() {
    let wire = json!({"status":"applied","receipt":{
        "admission":{"sessionId":"child", "admissionId":"ticket", "instanceId":"i", "generationId":"g",
        "lifecycle":2, "controlGeneration":3, "workId":"w", "workRevision":4,
        "execution":{"turnId":"00000000-0000-4000-8000-000000000001", "attemptId":"a"}},"evidenceId":"e",
    }});
    let receipt: SettlementOutcome = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(receipt).unwrap(), wire);
}

#[test]
fn existing_admission_is_explicit_and_optional_in_the_same_protocol() {
    let snapshot = crate::session_resources::work::WorkInspection {
        session_id: "s".into(),
        control: crate::session_resources::ControlState::default(),
        head: crate::session_resources::work::SessionWorkHead::default(),
        page: crate::session_resources::work::WorkPage::Availability(
            crate::session_resources::work::WorkAvailability {
                lifecycle: 1,
                change_seq: 0,
                blocked: false,
                pending: false,
                candidates: Vec::new(),
            },
        ),
        next_cursor: None,
    };
    let lean = AdmissionSnapshot::from(&snapshot);
    let wire = json!({"requestId":"request", "snapshot":lean});
    let request: AdmissionRequest = serde_json::from_value(wire.clone()).unwrap();
    assert!(request.existing_admission.is_none());
    assert_eq!(serde_json::to_value(request).unwrap(), wire);
    assert!(wire["snapshot"].get("state").is_none());
    assert!(wire["snapshot"].get("pendingCommands").is_none());
    assert!(serde_json::from_value::<AdmissionRequest>(json!({
        "requestId":"request", "snapshot":snapshot
    }))
    .is_err());
}
