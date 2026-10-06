use super::*;
use peri_acp_types::{
    execution_admission::{AdmissionRequest, AdmissionSnapshot},
    identity::AttemptId,
    messages::BaseMessage,
    session::{MessagePolicy, TurnId},
    session_resources::{
        work::{
            DeliveryPurpose, PublishDelivery, WorkAction, WorkAdmission, WorkCommand, WorkDecision,
            WorkEvent, WorkSelector,
        },
        ControlAttempt,
    },
};

#[derive(Default)]
struct AdmissionProbe {
    requests: std::sync::Mutex<Vec<AdmissionRequest>>,
}

#[async_trait]
impl crate::transport::RequestTransport for AdmissionProbe {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        assert_eq!(method, peri_acp_types::execution_admission::ADMIT_METHOD);
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::from_value(params).unwrap());
        Ok(json!({"status":"busy"}))
    }
}

#[tokio::test]
async fn work_execute_confirms_the_validated_availability_with_sdk_before_registration() {
    let tmp = tempfile::tempdir().unwrap();
    let configuration = make_peri_config_with_provider(make_provider_config(
        "test",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&configuration).unwrap();
    let mut cfg = make_server_config(configuration, provider, &tmp).await;
    let mut states = HashMap::new();
    let session_id =
        register_session_with_history(&mut states, tmp.path().to_str().unwrap(), &cfg).await;
    let content = crate::host::work_query::test_payload(
        cfg.session_resources.as_ref(),
        &session_id,
        &PersistedPayload::Message(BaseMessage::human("durable work")),
    )
    .await;
    let receipt = cfg
        .session_resources
        .apply_work_mutation(&WorkCommand {
            session_id: session_id.clone(),
            recipient_lifecycle: 1,
            mutation_id: "work-execute-input".into(),
            action: WorkAction::PublishDelivery {
                delivery: PublishDelivery {
                    delivery_id: "work-execute-delivery".into(),
                    event: WorkEvent {
                        producer_namespace: "execution-work-test".into(),
                        event_id: "work-execute-input".into(),
                        event_kind: "userInput".into(),
                        causation_id: None,
                        content,
                    },
                    purpose: DeliveryPurpose::UserInput,
                    policy: MessagePolicy::ensure_processing(),
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let available = crate::host::work_query::inspect(
        cfg.session_resources.as_ref(),
        &session_id,
        WorkSelector::Availability,
    )
    .await
    .unwrap();
    let expected = AdmissionSnapshot::from(&available);
    let candidate = &expected.candidates[0];
    let admission = WorkAdmission {
        session_id: session_id.clone(),
        admission_id: "work-execute-sdk-ticket".into(),
        instance_id: "work-execute-instance".into(),
        generation_id: "work-execute-generation".into(),
        lifecycle: expected.control.lifecycle,
        control_generation: expected.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    };
    expected.validate_admission(&admission).unwrap();
    let probe = Arc::new(AdmissionProbe::default());
    cfg.execution_admission_port = Some(Arc::new(
        crate::host::execution_admission::ReverseExecutionAdmission::new(probe.clone()),
    ));
    let cfg = Arc::new(cfg);
    let sessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let prompt_locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let continuation = Arc::new(sender);
    let error = crate::host::execution::execute(
        json!({"sessionId":session_id,"ticket":admission}),
        crate::host::execution::ExecutionContext {
            sessions: &sessions,
            prompt_locks: &prompt_locks,
            cfg: &cfg,
            transport: &transport,
            continuation: &continuation,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "SDK execution admission remains unconfirmed");
    let requests = probe.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].snapshot, expected);
    assert_eq!(requests[0].existing_admission.as_ref(), Some(&admission));
    requests[0].snapshot.validate_admission(&admission).unwrap();
    let snapshot = crate::host::work_query::inspect(
        cfg.session_resources.as_ref(),
        &session_id,
        WorkSelector::Admission {
            admission_id: admission.admission_id.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        crate::host::work_query::admission(&snapshot, &admission.admission_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn initial_prompt_attaches_mailbox_before_durable_publication_and_sdk_admission() {
    let tmp = tempfile::tempdir().unwrap();
    let configuration = make_peri_config_with_provider(make_provider_config(
        "test",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&configuration).unwrap();
    let mut cfg = make_server_config(configuration, provider, &tmp).await;
    let mut states = HashMap::new();
    let session_id =
        register_session_with_history(&mut states, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager
        .ensure_session(&session_id, tmp.path().to_str().unwrap());
    states.get_mut(&session_id).unwrap().frozen = Some(
        crate::host::requests::resource_owners::load_frozen_for_environment(&cfg, &session_id)
            .await
            .unwrap(),
    );
    let probe = Arc::new(AdmissionProbe::default());
    cfg.execution_admission_port = Some(Arc::new(
        crate::host::execution_admission::ReverseExecutionAdmission::new(probe.clone()),
    ));
    let sessions = Arc::new(tokio::sync::Mutex::new(states));
    let prompt_locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let result = crate::host::prompt_dispatch::dispatch_prompt_turn(
        json!({"sessionId":session_id,"requestId":"first-prompt","prompt":[{"type":"text","text":"first durable prompt"}]}),
        false, &sessions, &prompt_locks, &transport, &cfg, &sender,
    ).await;
    assert!(result.is_err());
    let requests = probe.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "{result:?}");
    assert_eq!(requests[0].snapshot.session_id, session_id);
    assert!(!requests[0].snapshot.blocked);
    assert_eq!(requests[0].snapshot.candidates.len(), 1);
    assert_eq!(requests[0].snapshot.candidates[0].delivery_ids.len(), 1);
    assert!(requests[0].existing_admission.is_none());
    let delivery = crate::host::work_query::test_delivery(
        cfg.session_resources.as_ref(),
        &session_id,
        &requests[0].snapshot.candidates[0].delivery_ids[0],
    )
    .await;
    assert_eq!(
        delivery.obligation,
        peri_acp_types::session_resources::work::ObligationStatus::Pending
    );
    let evidence = cfg
        .session_resources
        .read_evidence(&peri_acp_types::session_resources::work::EvidenceQuery {
            session_id: session_id.clone(),
            reference: delivery.publication.event.content.content.clone(),
        })
        .await
        .unwrap();
    assert!(String::from_utf8(evidence.bytes)
        .unwrap()
        .contains("first durable prompt"));
}
