use super::*;
use crate::agent::model_bridge::AgentModelBridge;
use crate::session::test_resources::TestSession;
use crate::session::{FrozenContext, MessageSource, Session};
use peri_acp_types::execution_admission::*;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::{ControlAttempt, SessionResources};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct ExplicitSdkFixture(Arc<dyn SessionResources>);

#[async_trait::async_trait]
impl ExecutionAdmissionPort for ExplicitSdkFixture {
    async fn admit(
        &self,
        _: AdmissionRequest,
    ) -> Result<AdmissionOutcome, ExecutionAdmissionError> {
        Err(ExecutionAdmissionError::Protocol(
            "issued fixture ticket must not be admitted twice".into(),
        ))
    }

    async fn entered(
        &self,
        request: EntryRequest,
    ) -> Result<EntryOutcome, ExecutionAdmissionError> {
        let snapshot = self
            .0
            .inspect_work(&WorkQuery::new(
                request.admission.session_id.clone(),
                WorkSelector::Availability,
            ))
            .await
            .map_err(|error| ExecutionAdmissionError::Protocol(error.to_string()))?;
        let registration =
            crate::session::work_access::admission(self.0.as_ref(), &request.admission)
                .await
                .map_err(|error| ExecutionAdmissionError::Protocol(error.to_string()))?;
        assert_eq!(registration.admission, request.admission);
        assert_eq!(registration.entering_mutation_id, request.entry_evidence_id);
        assert_eq!(
            snapshot.control.attempt.as_ref(),
            Some(&request.admission.execution)
        );
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
        panic!("stage loop cannot settle SDK admission before owner history and forwarder barriers")
    }
}

pub(super) struct ProductionFixture {
    pub(super) bound: TestSession,
    pub(super) context: StageContext,
    pub(super) admission: WorkAdmission,
    pub(super) delivery_id: String,
}

pub(super) async fn saved_processing(fixture: &ProductionFixture) -> Processing {
    crate::session::work_access::processing(
        fixture.bound.resources().as_ref(),
        &fixture.bound.thread_id(),
        &fixture.admission.work_id,
    )
    .await
    .unwrap()
}

pub(super) async fn saved_effects(fixture: &ProductionFixture) -> Vec<Effect> {
    let inspected = fixture
        .bound
        .resources()
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Effects {
                processing_id: fixture.admission.work_id.clone(),
                phase_sequence: None,
            },
        ))
        .await
        .unwrap();
    let WorkPage::Effects(effects) = inspected.page else {
        panic!("expected effect page")
    };
    assert!(inspected.next_cursor.is_none());
    effects
}

pub(super) async fn saved_delivery(fixture: &ProductionFixture) -> Delivery {
    let inspected = fixture
        .bound
        .resources()
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Delivery {
                delivery_id: fixture.delivery_id.clone(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Deliveries(mut deliveries) = inspected.page else {
        panic!("expected delivery page")
    };
    deliveries.pop().unwrap()
}

pub(super) async fn fixture(
    llm: Arc<dyn ReactLLM + Send + Sync>,
    tools: Vec<Arc<dyn BaseTool>>,
) -> ProductionFixture {
    fixture_with_input(llm, tools, &"full durable input ".repeat(5000)).await
}

pub(super) async fn fixture_with_input(
    llm: Arc<dyn ReactLLM + Send + Sync>,
    tools: Vec<Arc<dyn BaseTool>>,
    input: &str,
) -> ProductionFixture {
    let bound = TestSession::open().await;
    let session = Session::new(
        Arc::from("/tmp/rcra-production-test"),
        FrozenContext::builder().build(),
        None,
    );
    *session.transcript().write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    let initial = BaseMessage::human(input);
    session
        .queue()
        .push(QueuedMessage::prompt(MessageSource::UserInput, initial));
    publish_session_inbox(bound.resources(), &bound.thread_id, 1, session.queue())
        .await
        .unwrap();
    let snapshot = bound
        .resources
        .inspect_work(&WorkQuery::new(
            bound.thread_id(),
            WorkSelector::Availability,
        ))
        .await
        .unwrap();
    let WorkPage::Availability(availability) = &snapshot.page else {
        panic!("expected availability")
    };
    let candidate = &availability.candidates[0];
    let turn = session.start_turn();
    let execution = turn.execution_binding();
    let admission = WorkAdmission {
        session_id: bound.thread_id(),
        admission_id: uuid::Uuid::now_v7().to_string(),
        instance_id: "explicit-sdk-test-instance".into(),
        generation_id: "explicit-sdk-test-generation".into(),
        lifecycle: snapshot.control.lifecycle,
        control_generation: snapshot.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: ControlAttempt {
            turn_id: execution.turn_id,
            attempt_id: execution.attempt_id,
        },
    };
    let receipt = bound
        .resources
        .apply_work_mutation(&WorkCommand {
            session_id: bound.thread_id(),
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
    let tool_map = tools
        .into_iter()
        .map(|tool| (tool.name().to_owned(), tool))
        .collect();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_recipient_lifecycle(admission.lifecycle)
        .with_llm(llm)
        .with_execution_admission_port(Arc::new(ExplicitSdkFixture(bound.resources())))
        .with_tools(Arc::new(RwLock::new(tool_map)))
        .build();
    ProductionFixture {
        bound,
        context,
        admission,
        delivery_id: candidate.delivery_ids[0].clone(),
    }
}

pub(super) async fn native_model() -> (AgentModelBridge, tokio::net::TcpListener) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
        endpoint.parse().unwrap(),
        "never-persist-this-api-key",
        "frozen-model",
    ));
    (
        AgentModelBridge::new(Arc::new(model)).with_system("frozen system"),
        listener,
    )
}

pub(super) async fn read_http_body(socket: &mut tokio::net::TcpStream) -> Value {
    let mut request = Vec::new();
    let body_start;
    let content_length;
    loop {
        let mut chunk = [0_u8; 8192];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&chunk[..count]);
        if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            body_start = header_end + 4;
            break;
        }
    }
    while request.len() < body_start + content_length {
        let mut chunk = [0_u8; 8192];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&chunk[..count]);
    }
    let body: Value =
        serde_json::from_slice(&request[body_start..body_start + content_length]).unwrap();
    body
}

pub(super) fn serve(
    listener: tokio::net::TcpListener,
    resources: Arc<dyn SessionResources>,
    session_id: String,
    tools: bool,
) -> tokio::task::JoinHandle<Value> {
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = read_http_body(&mut socket).await;
        let snapshot = resources
            .inspect_work(&WorkQuery::new(&session_id, WorkSelector::Head))
            .await
            .unwrap();
        let work = crate::session::work_access::processing(
            resources.as_ref(),
            &session_id,
            snapshot.head.current_processing_id.as_deref().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(work.stage, WorkStage::ReasonInFlight);
        let checkpoint = resources
            .read_evidence(&EvidenceQuery {
                session_id,
                reference: work.request.clone().unwrap(),
            })
            .await
            .unwrap();
        checkpoint.validate().unwrap();
        let full: Value = serde_json::from_slice(&checkpoint.bytes).unwrap();
        assert_eq!(full["request"]["body"], body);
        assert!(!String::from_utf8(checkpoint.bytes)
            .unwrap()
            .contains("never-persist-this-api-key"));
        assert_eq!(work.budget.reason_requests, 1);
        let delta = if tools {
            json!({"role":"assistant","tool_calls":[{"index":0,"id":"call-native","type":"function","function":{"name":"probe","arguments":"{\"value\":\"exact\"}"}}]})
        } else {
            json!({"role":"assistant","content":"durable answer"})
        };
        let first = json!({"id":"native-request","model":"frozen-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
        let finish = json!({"id":"native-request","choices":[{"index":0,"delta":{},"finish_reason":if tools {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}});
        let response = format!("data: {first}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
        let headers = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", response.len());
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(response.as_bytes()).await.unwrap();
        body
    })
}
