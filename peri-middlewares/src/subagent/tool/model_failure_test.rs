//! Cross-layer model failure fixtures.
//!
//! These tests inject failures at the `peri_model::Model` boundary, then run
//! the real AgentModelBridge, SubAgentTool, child executor, and parent tool
//! dispatch.  They intentionally inspect the parent's canonical tool message
//! instead of constructing a safe failure projection by hand.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{atomic::AtomicUsize, Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::event::{
    AgentEventHandler, BackgroundTaskResult, ExecutorEvent, FnEventHandler,
};
use peri_acp_types::event_v2::{ObserveEvent, RenderEvent};
use peri_acp_types::identity::AgentId;
use peri_agent::agent::model_bridge::AgentModelBridge;
use peri_agent::agent::react::{ReactLLM, Reasoning, StreamingContext};
use peri_agent::agent::stages::{run_react_loop, LoopResult, StageContext};
use peri_agent::messages::BaseMessage;
use peri_agent::session::queue::{MessageSource, QueuedMessage};
use peri_agent::session::store::FrozenContext;
use peri_agent::session::Session;
use peri_agent::tools::BaseTool;
use peri_model::{
    Model, ModelCapabilities, ModelRequest, ModelResult, ModelStream, OpenAiConfig, OpenAiModel,
    PreparedModelRequest, RetryConfig,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::SubAgentTool;

#[derive(Clone, Copy)]
enum FailureFixture {
    Http(u16),
    VisibleDeltaThenInterruption,
}

/// A loopback-only provider boundary keeps these fixtures on the public model
/// API while exercising the real OpenAI adapter and ModelRuntime retry stream.
/// It never contacts the network or reads credentials from the environment.
struct LocalProvider {
    address: SocketAddr,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl LocalProvider {
    async fn start(fixture: FailureFixture) -> Arc<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback model fixture");
        let address = listener.local_addr().expect("loopback address");
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_task = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                requests_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut request = [0_u8; 16 * 1024];
                let _ = socket.read(&mut request).await;
                let (status, body, request_id, content_type) = match fixture {
                    FailureFixture::Http(status) => (
                        status,
                        String::new(),
                        format!("req-{status}"),
                        "application/json",
                    ),
                    FailureFixture::VisibleDeltaThenInterruption => (
                        200,
                        "data: {\"id\":\"resp-stream\",\"choices\":[{\"delta\":{\"content\":\"visible-before-interruption\"},\"finish_reason\":null}]}\n\n".to_string(),
                        "req-stream".to_string(),
                        "text/event-stream",
                    ),
                };
                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    _ => "Fixture Error",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\nx-request-id: {request_id}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Arc::new(Self {
            address,
            requests,
            task,
        })
    }

    fn endpoint(&self) -> url::Url {
        url::Url::parse(&format!("http://{}/v1", self.address)).expect("fixture endpoint")
    }

    fn request_count(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for LocalProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct RuntimeFailureModel {
    inner: OpenAiModel,
    _provider: Arc<LocalProvider>,
}

impl RuntimeFailureModel {
    async fn new(fixture: FailureFixture) -> (Self, Arc<LocalProvider>) {
        let provider = LocalProvider::start(fixture).await;
        let runtime = peri_model::ModelRuntimeConfig::default().with_retry(
            RetryConfig::default()
                .with_base_delay(Duration::ZERO)
                .with_max_delay(Duration::ZERO)
                .with_jitter(false),
        );
        let config = OpenAiConfig::new(provider.endpoint(), "fixture-key", "fixture-model")
            .with_runtime(runtime);
        let inner = OpenAiModel::new(config);
        (
            Self {
                inner,
                _provider: Arc::clone(&provider),
            },
            provider,
        )
    }
}

#[async_trait]
impl Model for RuntimeFailureModel {
    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn prepare_request(&self, request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        self.inner.prepare_request(request)
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        self.inner.stream(request, cancellation).await
    }
}

struct ParentDriver {
    calls: std::sync::atomic::AtomicUsize,
    seen: Arc<Mutex<Vec<Vec<BaseMessage>>>>,
    input: serde_json::Value,
}

struct RecordingBridge {
    observes: Arc<Mutex<Vec<ObserveEvent>>>,
}

impl peri_agent::agent::LangfuseBridgeLike for RecordingBridge {
    fn process_render_event(&self, _event: &peri_agent::agent::events_v2::RenderEvent) {}

    fn process_observe_event(&self, event: &peri_agent::agent::events_v2::ObserveEvent) {
        self.observes.lock().unwrap().push(event.clone());
    }
}

#[async_trait]
impl ReactLLM for ParentDriver {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        self.seen.lock().unwrap().push(messages.to_vec());
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Ok(Reasoning::with_tools(
                "delegate",
                vec![peri_agent::agent::react::ToolCall::new(
                    "parent-agent-call",
                    "Agent",
                    self.input.clone(),
                )],
            ))
        } else {
            Ok(Reasoning::with_answer("", "parent completed"))
        }
    }
}

struct SyncFixture {
    tool_message: serde_json::Value,
    parent_model_messages: Vec<BaseMessage>,
    child_events: Vec<ExecutorEvent>,
}

async fn run_sync_fixture(fixture: FailureFixture, status: Option<u16>) -> SyncFixture {
    let dir = tempfile::tempdir().expect("fixture directory");
    write_agent(&dir);
    let cwd = dir.path().to_string_lossy().into_owned();

    let child_events = Arc::new(Mutex::new(Vec::new()));
    let child_events_for_handler = Arc::clone(&child_events);
    let child_handler: Arc<dyn AgentEventHandler> = Arc::new(FnEventHandler(move |event| {
        child_events_for_handler.lock().unwrap().push(event);
    }));
    let (runtime_model, provider) = RuntimeFailureModel::new(fixture).await;
    let model: Arc<dyn Model> = Arc::new(runtime_model);
    let model_for_factory = Arc::clone(&model);
    let bridge = Arc::new(RecordingBridge {
        observes: Arc::new(Mutex::new(Vec::new())),
    });
    let child_tool: Arc<dyn BaseTool> =
        Arc::new(
            SubAgentTool::new(
                Arc::new(Vec::new()),
                Some(child_handler),
                Arc::new(move |_| {
                    Box::new(AgentModelBridge::from_arc(Arc::clone(&model_for_factory)))
                        as Box<dyn ReactLLM + Send + Sync>
                }),
                cwd.clone(),
            )
            .with_parent_agent_id(Arc::new(RwLock::new(Some(AgentId::new()))))
            .with_langfuse_bridge(
                Arc::clone(&bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>
            ),
        );

    let input = serde_json::json!({
        "subagent_type": "fixture-agent",
        "prompt": "exercise model failure",
        "cwd": cwd,
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        None,
    );
    let turn = parent.start_turn();
    let transcript = parent.transcript();
    let queue = parent.queue().clone();
    let shared_tools = Arc::new(RwLock::new(BTreeMap::from([(
        "Agent".to_string(),
        child_tool,
    )])));
    let (event_bus, mut event_handles) =
        peri_agent::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(turn, transcript, queue.clone())
        .with_llm(Arc::new(ParentDriver {
            calls: std::sync::atomic::AtomicUsize::new(0),
            seen: Arc::clone(&seen),
            input,
        }))
        .with_tools(shared_tools)
        .with_event_bus(Arc::new(event_bus))
        .build();
    queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("parent request"),
    ));

    assert!(matches!(
        run_react_loop(context.clone(), 3).await,
        LoopResult::Completed
    ));
    let expected_requests = if status.is_some_and(|status| status == 400)
        || matches!(fixture, FailureFixture::VisibleDeltaThenInterruption)
    {
        1
    } else {
        6
    };
    assert_eq!(provider.request_count(), expected_requests);
    let tool_message = {
        let transcript = context.session.transcript.read();
        let messages = transcript.visible_messages();
        messages
            .iter()
            .map(|message| serde_json::to_value(message).expect("canonical message JSON"))
            .find(|message| message["role"] == "tool")
            .expect("parent canonical tool result")
    };
    let decoded_tool_message: BaseMessage =
        serde_json::from_value(tool_message.clone()).expect("canonical tool message JSON");
    assert_eq!(
        serde_json::to_value(&decoded_tool_message).expect("canonical tool message re-JSON"),
        tool_message,
        "canonical BaseMessage must survive serialize/deserialize with typed failure facts"
    );
    assert_eq!(tool_message["is_error"], true);
    if let Some(status) = status {
        assert_eq!(
            tool_message["subagent_failure"]["diagnostic"]["status"],
            status
        );
    } else {
        assert!(tool_message["subagent_failure"]["diagnostic"]["status"].is_null());
    }
    assert_eq!(
        tool_message["subagent_failure"]["diagnostic"]["provider"],
        "openai-compatible"
    );
    assert!(tool_message["subagent_failure"]["child_thread_id"]
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_some());

    // The second parent model request is the actual model-visible projection.
    let parent_model_messages = seen.lock().unwrap().get(1).cloned().unwrap_or_default();
    if status.is_some() {
        assert!(parent_model_messages
            .iter()
            .any(|message| message.content().contains("model_error_status")));
    }
    if status.is_some() {
        assert!(parent_model_messages
            .iter()
            .any(|message| message.content().contains("model_error_request_id")));
    }
    assert!(!parent_model_messages
        .iter()
        .any(|message| message.content().contains("provider body")));

    let render_failure = loop {
        if let Some(event) = event_handles.try_render() {
            if let RenderEvent::ToolEnded {
                is_error: true,
                subagent_failure: Some(failure),
                ..
            } = event
            {
                break Some(failure);
            }
        } else {
            break None;
        }
    };
    assert_eq!(
        render_failure.map(|failure| serde_json::to_value(failure).expect("render failure JSON")),
        Some(tool_message["subagent_failure"].clone()),
        "live ToolEnded must preserve the canonical typed child failure"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    let stop_failure = loop {
        if let Some(failure) =
            bridge
                .observes
                .lock()
                .unwrap()
                .iter()
                .find_map(|event| match event {
                    ObserveEvent::SubagentStop {
                        subagent_failure: Some(failure),
                        ..
                    } => Some(failure.clone()),
                    _ => None,
                })
        {
            break Some(failure);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(
        stop_failure.map(|failure| serde_json::to_value(failure).expect("stop failure JSON")),
        Some(tool_message["subagent_failure"].clone()),
        "live SubagentStop must preserve the canonical typed child failure"
    );

    // Give the child EventBus forwarder one bounded scheduling window to drain.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let child_events = child_events.lock().unwrap().clone();
    let stop_positions: Vec<_> = child_events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event, ExecutorEvent::SubagentStopped { .. }).then_some(index)
        })
        .collect();
    assert_eq!(
        stop_positions.len(),
        1,
        "each child execution must emit exactly one terminal stop"
    );
    let start_position = child_events
        .iter()
        .position(|event| matches!(event, ExecutorEvent::SubagentStarted { .. }))
        .expect("child start event");
    assert!(
        start_position < stop_positions[0],
        "child stop must follow child start"
    );
    if matches!(fixture, FailureFixture::VisibleDeltaThenInterruption) {
        let delta_position = child_events
            .iter()
            .position(|event| matches!(event, ExecutorEvent::TextChunk { .. }))
            .expect("visible delta event");
        assert!(
            delta_position < stop_positions[0],
            "visible output must precede the terminal stop: events={child_events:?}"
        );
    }
    SyncFixture {
        tool_message,
        parent_model_messages,
        child_events,
    }
}

fn write_agent(dir: &TempDir) {
    let agents = dir.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::write(
        agents.join("fixture-agent.md"),
        "---\nname: fixture-agent\ndescription: failure fixture\n---\n\nRun the failure fixture.\n",
    )
    .expect("agent definition");
}

#[tokio::test]
async fn sync_http_400_reaches_parent_canonical_result_with_safe_identity() {
    let fixture = run_sync_fixture(FailureFixture::Http(400), Some(400)).await;
    assert!(
        fixture.tool_message["subagent_failure"]["diagnostic"]["request_id"]
            .as_str()
            .is_some()
    );
    assert!(!fixture
        .parent_model_messages
        .iter()
        .any(|message| message.content().contains("prompt")));
}

#[tokio::test]
async fn sync_http_429_reaches_parent_canonical_result_with_safe_identity() {
    let fixture = run_sync_fixture(FailureFixture::Http(429), Some(429)).await;
    assert_eq!(
        fixture.tool_message["subagent_failure"]["diagnostic"]["category"],
        "http_status"
    );
    assert_eq!(
        fixture.tool_message["subagent_failure"]["diagnostic"]["retry_attempts"],
        6
    );
}

#[tokio::test]
async fn sync_http_500_reaches_parent_canonical_result_with_safe_identity() {
    let fixture = run_sync_fixture(FailureFixture::Http(500), Some(500)).await;
    assert_eq!(
        fixture.tool_message["subagent_failure"]["diagnostic"]["request_id"],
        "req-500"
    );
}

#[tokio::test]
async fn visible_delta_then_interruption_reaches_parent_and_forwards_delta() {
    let fixture = run_sync_fixture(FailureFixture::VisibleDeltaThenInterruption, None).await;
    let delta = fixture.child_events.iter().find_map(|event| match event {
        ExecutorEvent::TextChunk { chunk, .. } => Some(chunk.as_str()),
        _ => None,
    });
    assert_eq!(delta, Some("visible-before-interruption"));
    assert_eq!(
        fixture.tool_message["subagent_failure"]["diagnostic"]["category"],
        "stream_interrupted"
    );
    assert!(fixture.tool_message["subagent_failure"]["diagnostic"]["request_id"].is_null());
}

#[tokio::test]
async fn background_http_429_consumes_typed_result_and_safe_notification() {
    let dir = tempfile::tempdir().expect("fixture directory");
    write_agent(&dir);
    let cwd = dir.path().to_string_lossy().into_owned();
    let (runtime_model, provider) = RuntimeFailureModel::new(FailureFixture::Http(429)).await;
    let model: Arc<dyn Model> = Arc::new(runtime_model);
    let model_for_factory = Arc::clone(&model);
    let (bg_event_tx, mut bg_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let completed_tx = Arc::new(Mutex::new(Some(completed_tx)));
    let completed_tx_for_callback = Arc::clone(&completed_tx);
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(move |_| {
            Box::new(AgentModelBridge::from_arc(Arc::clone(&model_for_factory)))
                as Box<dyn ReactLLM + Send + Sync>
        }),
        cwd.clone(),
    )
    .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()))
    .with_bg_event_sender(bg_event_tx)
    .with_on_bg_complete(Arc::new(move |result, _kind| {
        if let Some(sender) = completed_tx_for_callback.lock().unwrap().take() {
            let _ = sender.send(result.clone());
        }
    }));

    let launch = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "fixture-agent",
                "prompt": "background failure",
                "run_in_background": true,
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("background task should register before model execution");
    assert!(launch.contains("Background"));

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("background completion")
        .expect("completion callback");
    assert!(!result.success);
    assert!(!result.timed_out);
    let failure = result
        .subagent_failure
        .as_ref()
        .expect("background result keeps typed child failure");
    assert_eq!(failure.diagnostic().status(), Some(429));
    assert_eq!(failure.diagnostic().request_id(), Some("req-429"));
    assert_eq!(provider.request_count(), 6);
    let encoded_result = serde_json::to_vec(&result).expect("background result JSON");
    let decoded_result: BackgroundTaskResult =
        serde_json::from_slice(&encoded_result).expect("background result roundtrip");
    assert_eq!(decoded_result.child_thread_id, result.child_thread_id);
    let decoded_failure = decoded_result
        .subagent_failure
        .as_ref()
        .expect("roundtrip keeps background safe failure");
    assert_eq!(decoded_failure.diagnostic().status(), Some(429));
    assert_eq!(
        decoded_result.to_notification(),
        result.to_notification(),
        "safe notification must survive the typed result serde boundary"
    );
    let mut lifecycle = Vec::new();
    while let Ok(event) = bg_event_rx.try_recv() {
        lifecycle.push(event);
    }
    let start_position = lifecycle
        .iter()
        .position(|event| matches!(event, ExecutorEvent::SubagentStarted { .. }))
        .expect("background child start event");
    let stop_positions: Vec<_> = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event, ExecutorEvent::SubagentStopped { .. }).then_some(index)
        })
        .collect();
    assert_eq!(
        stop_positions.len(),
        1,
        "background child must stop exactly once"
    );
    assert!(start_position < stop_positions[0]);
    if let ExecutorEvent::SubagentStopped {
        subagent_failure: Some(stop_failure),
        is_error: true,
        ..
    } = &lifecycle[stop_positions[0]]
    {
        assert_eq!(stop_failure.diagnostic().status(), Some(429));
        assert_eq!(stop_failure.diagnostic().request_id(), Some("req-429"));
    } else {
        panic!("background error stop must carry its safe failure facts");
    }
    // The error branch delivers the terminal BackgroundTaskResult through the
    // callback/TaskManager after SubagentStopped; it intentionally does not
    // synthesize a BackgroundTaskCompleted event on the event channel.
    assert!(!lifecycle
        .iter()
        .any(|event| matches!(event, ExecutorEvent::BackgroundTaskCompleted(_))));
    let notification = result.to_notification();
    assert!(notification.contains("model_error_status: 429"));
    assert!(notification.contains("model_error_request_id: req-429"));
    assert!(!notification.contains("provider body"));
}
