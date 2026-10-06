use super::*;
use peri_acp_types::execution_admission::*;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::ControlAttempt;
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::stages::{publish_session_inbox, run_react_loop, LoopResult, StageContext};
use peri_agent::session::{
    FrozenContext, MessageSource, MessageTranscript, QueuedMessage, Session,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const MISSING_THREAD: &str = "00000000-0000-0000-0000-000000000000";

struct ExplicitSdkFixture {
    resources: Arc<dyn SessionResources>,
    entries: AtomicUsize,
}

#[async_trait::async_trait]
impl ExecutionAdmissionPort for ExplicitSdkFixture {
    async fn admit(
        &self,
        _: AdmissionRequest,
    ) -> Result<AdmissionOutcome, ExecutionAdmissionError> {
        panic!("registered parent admission must not be issued twice")
    }

    async fn entered(
        &self,
        request: EntryRequest,
    ) -> Result<EntryOutcome, ExecutionAdmissionError> {
        let snapshot = self
            .resources
            .inspect_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                selector: WorkSelector::Admission {
                    admission_id: request.admission.admission_id.clone(),
                },
                limit: 1,
                cursor: None,
            })
            .await
            .unwrap();
        let WorkPage::Admissions(records) = &snapshot.page else {
            panic!("admission page expected")
        };
        let registration = &records[0];
        assert_eq!(registration.admission, request.admission);
        assert_eq!(registration.entering_mutation_id, request.entry_evidence_id);
        assert_eq!(
            snapshot.control.attempt.as_ref(),
            Some(&request.admission.execution)
        );
        self.entries.fetch_add(1, Ordering::SeqCst);
        Ok(EntryOutcome::Applied {
            receipt: EntryReceipt {
                admission: request.admission,
                entry_evidence_id: request.entry_evidence_id,
            },
        })
    }

    async fn settle(
        &self,
        _: SettlementRequest,
    ) -> Result<SettlementOutcome, ExecutionAdmissionError> {
        panic!("outer stage loop cannot settle admission before owner barriers")
    }
}

struct MissingThreadParent {
    resources: Arc<dyn SessionResources>,
    parent_id: String,
    requests: Mutex<Vec<Vec<BaseMessage>>>,
}

#[async_trait::async_trait]
impl ReactLLM for MissingThreadParent {
    fn prepare_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> peri_agent::error::AgentResult<peri_model::PreparedModelCall> {
        Ok(peri_model::PreparedModelCall::new(
            serde_json::json!({
                "provider": "resume-regression",
                "model": "parent-fixture",
                "endpoint": "http://127.0.0.1:1",
                "credentialRef": "fixture-no-credentials",
                "body": {
                    "messages": messages,
                    "tools": tools.iter().map(|tool| tool.definition()).collect::<Vec<_>>(),
                },
            }),
            |_| panic!("fixture reasoning has no network send"),
        ))
    }

    async fn generate_prepared_reasoning(
        &self,
        prepared: peri_model::PreparedModelCall,
        streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let messages: Vec<BaseMessage> =
            serde_json::from_value(prepared.checkpoint()["body"]["messages"].clone()).unwrap();
        self.generate_reasoning(&messages, &[], streaming).await
    }

    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let snapshot = self
            .resources
            .inspect_work(&WorkQuery {
                session_id: self.parent_id.clone(),
                selector: WorkSelector::CurrentProcessing,
                limit: 1,
                cursor: None,
            })
            .await
            .unwrap();
        let WorkPage::Processings(processings) = &snapshot.page else {
            panic!("processing page expected")
        };
        assert!(processings
            .iter()
            .any(|processing| processing.stage == WorkStage::ReasonInFlight
                && processing.request.is_some()));
        let request_count = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(messages.to_vec());
            requests.len()
        };
        match request_count {
            1 => Ok(Reasoning::with_tools(
                "resume missing child",
                vec![ToolCall::new(
                    "missing-thread-resume",
                    "Agent",
                    serde_json::json!({
                        "resume_thread_id": MISSING_THREAD,
                        "prompt": "continue the child",
                    }),
                )],
            )),
            2 => {
                let tool_result = messages
                    .iter()
                    .find_map(|message| {
                        let value = serde_json::to_value(message).unwrap();
                        (value["role"] == "tool").then_some(value)
                    })
                    .expect("second Reason must receive the failed Agent tool result");
                assert_eq!(tool_result["is_error"], true);
                assert!(tool_result.to_string().contains("thread not found"));
                assert!(tool_result.to_string().contains(MISSING_THREAD));
                let effects = self
                    .resources
                    .inspect_work(&WorkQuery::new(
                        &self.parent_id,
                        WorkSelector::Effects {
                            processing_id: processings[0].processing_id.clone(),
                            phase_sequence: None,
                        },
                    ))
                    .await
                    .unwrap();
                let WorkPage::Effects(effects) = &effects.page else {
                    panic!("effects page expected")
                };
                assert_eq!(effects.len(), 1);
                let invocation = &effects[0];
                assert_eq!(invocation.status, InvocationStatus::Settled);
                assert!(matches!(
                    invocation.outcome,
                    Some(InvocationOutcome::Failed { .. })
                ));
                assert!(invocation.unknown_reason.is_none());
                Ok(Reasoning::with_answer(
                    "",
                    "parent continues after missing child",
                ))
            }
            _ => panic!("parent should finish on its second Reason"),
        }
    }
}

#[tokio::test]
async fn durable_parent_completes_after_real_agent_resume_missing_thread() {
    let directory = tempdir().unwrap();
    let fixture = SessionFixture::open_in(directory.path()).await;
    let cwd = fixture.workspace_cwd();
    let parent_id = fixture
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .unwrap();
    let parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        None,
    );
    *parent.transcript().write() =
        MessageTranscript::new().with_persistence(fixture.facade(), parent_id.clone());
    let control = fixture
        .resources
        .load_session_control(&parent_id)
        .await
        .unwrap();
    parent.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("resume the missing child and report back"),
    ));
    publish_session_inbox(
        fixture.facade(),
        &parent_id,
        control.lifecycle,
        parent.queue(),
    )
    .await
    .unwrap();
    let snapshot = fixture
        .resources
        .inspect_work(&WorkQuery {
            session_id: parent_id.clone(),
            selector: WorkSelector::Availability,
            limit: 1,
            cursor: None,
        })
        .await
        .unwrap();
    let WorkPage::Availability(availability) = &snapshot.page else {
        panic!("availability page expected")
    };
    assert_eq!(availability.candidates.len(), 1);
    let candidate = &availability.candidates[0];
    let turn = parent.start_turn();
    let execution = turn.execution_binding();
    let admission = WorkAdmission {
        session_id: parent_id.clone(),
        admission_id: uuid::Uuid::now_v7().to_string(),
        instance_id: "resume-regression-instance".into(),
        generation_id: "resume-regression-generation".into(),
        lifecycle: snapshot.control.lifecycle,
        control_generation: snapshot.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: ControlAttempt {
            turn_id: execution.turn_id,
            attempt_id: execution.attempt_id,
        },
    };
    let receipt = fixture
        .resources
        .apply_work_mutation(&WorkCommand {
            session_id: parent_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: format!("register:{}", admission.admission_id),
            action: WorkAction::RegisterAdmission {
                admission: admission.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    assert!(turn.bind_work_admission(admission.clone()));
    assert!(turn.bind_control_generation(admission.control_generation));
    let sdk = Arc::new(ExplicitSdkFixture {
        resources: fixture.facade(),
        entries: AtomicUsize::new(0),
    });
    let model = Arc::new(MissingThreadParent {
        resources: fixture.facade(),
        parent_id: parent_id.clone(),
        requests: Mutex::new(Vec::new()),
    });
    let tool: Arc<dyn BaseTool> = Arc::new(
        SubAgentTool::new(
            Arc::new(Vec::new()),
            None,
            Arc::new(|_| panic!("missing child must never construct a child model")),
            cwd,
        )
        .with_session_resources(fixture.facade())
        .with_parent_thread_id(parent_id.clone()),
    );
    let context = StageContext::builder(turn, parent.transcript(), parent.queue().clone())
        .with_recipient_lifecycle(admission.lifecycle)
        .with_execution_admission_port(sdk.clone())
        .with_llm(model.clone())
        .with_tools(Arc::new(RwLock::new(BTreeMap::from([(
            "Agent".into(),
            tool,
        )]))))
        .build();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_react_loop(context, 3),
    )
    .await
    .expect("durable parent must not stall on missing-thread failure");
    assert!(
        matches!(result, LoopResult::Completed),
        "parent loop result: {result:?}"
    );
    assert_eq!(sdk.entries.load(Ordering::SeqCst), 1);
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    let persisted = fixture
        .resources
        .inspect_work(&WorkQuery {
            session_id: parent_id.clone(),
            selector: WorkSelector::Processing {
                processing_id: admission.work_id.clone(),
            },
            limit: 1,
            cursor: None,
        })
        .await
        .unwrap();
    let WorkPage::Processings(processings) = &persisted.page else {
        panic!("processing page expected")
    };
    assert_eq!(processings.len(), 1);
    assert_eq!(processings[0].stage, WorkStage::Settled);
    let effects = fixture
        .resources
        .inspect_work(&WorkQuery::new(
            &parent_id,
            WorkSelector::Effects {
                processing_id: admission.work_id.clone(),
                phase_sequence: None,
            },
        ))
        .await
        .unwrap();
    let WorkPage::Effects(effects) = &effects.page else {
        panic!("effects page expected")
    };
    assert_eq!(effects.len(), 1);
    let invocation = &effects[0];
    assert_eq!(invocation.status, InvocationStatus::Settled);
    let Some(InvocationOutcome::Failed { result }) = &invocation.outcome else {
        panic!("missing-thread invocation must settle Failed, not OutcomeUnknown");
    };
    let evidence = fixture
        .resources
        .read_evidence(&EvidenceQuery {
            session_id: parent_id.clone(),
            reference: result.content.clone(),
        })
        .await
        .unwrap();
    evidence.validate().unwrap();
    assert!(String::from_utf8(evidence.bytes)
        .unwrap()
        .contains("thread not found"));
    assert!(invocation.unknown_reason.is_none());
    let history = fixture
        .resources
        .load_session_history(&parent_id)
        .await
        .unwrap();
    let history_messages: Vec<_> = history
        .iter()
        .filter_map(|payload| match payload {
            PersistedPayload::Message(message) => Some(message),
            PersistedPayload::SystemReminder { .. } => None,
        })
        .collect();
    let history_json = serde_json::to_string(&history_messages).unwrap();
    assert!(history_json.contains("thread not found"));
    assert!(history_json.contains("parent continues after missing child"));
}
