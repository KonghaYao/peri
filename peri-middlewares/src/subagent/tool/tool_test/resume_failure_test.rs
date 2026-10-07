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
            .load_session_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                limit: 10,
            })
            .await
            .unwrap();
        let registration = &snapshot.state.admissions[&request.admission.admission_id];
        assert_eq!(registration.admission, request.admission);
        assert_eq!(
            registration.entering_receipt.as_ref().unwrap().mutation_id,
            request.entry_evidence_id
        );
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

#[derive(Clone)]
struct MissingThreadParent {
    resources: Arc<dyn SessionResources>,
    parent_id: String,
    requests: Arc<Mutex<Vec<Vec<BaseMessage>>>>,
}

impl MissingThreadParent {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::subagent::test_support::*;
        let _ = &cancellation;
        let messages = base_messages(&request);
        let defined = defined_tools(&request);
        let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

        let snapshot = self
            .resources
            .load_session_work(&WorkQuery {
                session_id: self.parent_id.clone(),
                limit: 10,
            })
            .await
            .unwrap();
        assert!(snapshot.state.works.values().any(|work| {
            work.stage == WorkStage::ReasonInFlight && work.reason_request.is_some()
        }));
        let mut requests = self.requests.lock().unwrap();
        requests.push(messages.to_vec());
        match requests.len() {
            1 => tool_events_from_react(vec![ToolCall::new(
                "missing-thread-resume",
                "Agent",
                serde_json::json!({
                    "resume_thread_id": MISSING_THREAD,
                    "prompt": "continue the child",
                }),
            )]),
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
                assert_eq!(snapshot.state.invocations.len(), 1);
                let invocation = snapshot.state.invocations.values().next().unwrap();
                assert_eq!(invocation.status, InvocationStatus::Settled);
                assert!(matches!(
                    invocation.outcome,
                    Some(InvocationOutcome::Failed { .. })
                ));
                assert!(invocation.unknown_reason.is_none());
                text_events("parent continues after missing child")
            }
            _ => panic!("parent should finish on its second Reason"),
        }
    }
}
crate::subagent::test_support::fixture_model_impl!(MissingThreadParent);

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
        .load_session_work(&WorkQuery {
            session_id: parent_id.clone(),
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.candidates.len(), 1);
    let candidate = &snapshot.candidates[0];
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
        requests: Arc::new(Mutex::new(Vec::new())),
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
        .with_llm(Arc::new(
            peri_agent::agent::model_bridge::AgentModelBridge::new(
                Arc::clone(&model) as Arc<dyn peri_model::Model>
            ),
        ))
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
        .load_session_work(&WorkQuery {
            session_id: parent_id.clone(),
            limit: 10,
        })
        .await
        .unwrap();
    assert!(!persisted.state.works.is_empty());
    assert!(persisted
        .state
        .works
        .values()
        .all(|work| work.stage == WorkStage::Settled));
    assert_eq!(persisted.state.invocations.len(), 1);
    let invocation = persisted.state.invocations.values().next().unwrap();
    assert_eq!(invocation.status, InvocationStatus::Settled);
    let Some(InvocationOutcome::Failed { result }) = &invocation.outcome else {
        panic!("missing-thread invocation must settle Failed, not OutcomeUnknown");
    };
    assert!(result.serialized.contains("thread not found"));
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
