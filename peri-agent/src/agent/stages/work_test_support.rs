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
            .load_session_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ExecutionAdmissionError::Protocol(error.to_string()))?;
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
        .load_session_work(&WorkQuery {
            session_id: bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let candidate = &snapshot.candidates[0];
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
        .apply_work_mutation(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
                session_id: bound.thread_id(),
                recipient_lifecycle: admission.lifecycle,
                mutation_id: format!("register:{}", admission.admission_id),
                action: WorkAction::RegisterAdmission {
                    admission: admission.clone(),
                },
            })
            .unwrap(),
        )
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
        delivery_id: candidate.work_id.clone(),
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
            .load_session_work(&WorkQuery {
                session_id,
                limit: 1,
            })
            .await
            .unwrap();
        let work = snapshot
            .state
            .works
            .values()
            .find(|work| work.stage == WorkStage::ReasonInFlight)
            .unwrap();
        let checkpoint = work.reason_request.as_ref().unwrap();
        let full: Value = serde_json::from_str(&checkpoint.serialized_request).unwrap();
        assert_eq!(full["request"]["body"], body);
        assert!(!checkpoint
            .serialized_request
            .contains("never-persist-this-api-key"));
        assert_eq!(snapshot.state.budgets[&work.budget_id].reason_requests, 1);
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
