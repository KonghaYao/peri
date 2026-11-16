//! Cross-layer model failure fixtures.
//!
//! These tests inject failures at the `peri_model::Model` boundary, then run
//! the real AgentModelBridge, SubAgentTool, child executor, and parent tool
//! dispatch.  They intentionally inspect the parent's canonical tool message
//! instead of constructing a safe failure projection by hand.

use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::event::{BackgroundTaskResult, ExecutorEvent};
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

impl FailureFixture {
    fn expected_status(self) -> Option<u16> {
        match self {
            Self::Http(status) => Some(status),
            Self::VisibleDeltaThenInterruption => None,
        }
    }

    fn expected_request_id(self) -> Option<String> {
        self.expected_status().map(|status| format!("req-{status}"))
    }

    /// HTTP 400 不可重试：单次请求即失败。其余场景走满
    /// `RetryConfig::max_attempts`——含「可见增量后中断」：该路径不再整轮
    /// 作废，而是保留部分输出续跑，1 次初始请求 + 9 次续跑 = 10 次后耗尽。
    fn expected_requests(self) -> usize {
        if matches!(self, Self::Http(400)) {
            1
        } else {
            10
        }
    }
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

fn write_agent(dir: &TempDir) {
    let agents = dir.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::write(
        agents.join("fixture-agent.md"),
        "---\nname: fixture-agent\ndescription: failure fixture\n---\n\nRun the failure fixture.\n",
    )
    .expect("agent definition");
}

struct BackgroundFailure {
    result: BackgroundTaskResult,
    lifecycle: Vec<ExecutorEvent>,
    provider: Arc<LocalProvider>,
}

impl BackgroundFailure {
    fn diagnostic(&self) -> &peri_acp_types::error::SafeModelErrorDiagnostic {
        self.result
            .subagent_failure
            .as_ref()
            .expect("background result keeps typed child failure")
            .diagnostic()
            .expect("model diagnostic")
    }
}

/// 跑一次后台委派并收集终态结果、生命周期事件与提供方请求计数。
///
/// v5 起 Agent 只有后台执行模式；同步路径的失败断言按同一模型边界迁移到
/// 后台终态（`BackgroundTaskResult` / `SubagentStopped` / 完成通知）。
async fn run_background_fixture(fixture: FailureFixture) -> BackgroundFailure {
    let dir = tempfile::tempdir().expect("fixture directory");
    write_agent(&dir);
    // W5：Agent 定义来自会话绑定的资源面（真实 workspace 实例夹具）。
    let bg_face = crate::mcp::agent_face_fixture::AgentFaceFixture::connect(dir.path()).await;
    let bg_agent_registry = std::sync::Arc::clone(&bg_face.registry);
    let _bg_face_guard = bg_face;
    let cwd = dir.path().to_string_lossy().into_owned();
    let (runtime_model, provider) = RuntimeFailureModel::new(fixture).await;
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
            crate::subagent::test_support::fixture_source(
                Arc::clone(&model_for_factory),
                "fixture-scripted",
            )
        }),
        cwd.clone(),
    )
    .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()))
    .with_bg_event_sender(bg_event_tx)
    .with_mcp_agents(Some(Arc::clone(&bg_agent_registry)), None)
    .with_on_bg_complete(Arc::new(move |result, _kind| {
        if let Some(sender) = completed_tx_for_callback.lock().unwrap().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
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

    let result = tokio::time::timeout(Duration::from_secs(5), completed_rx)
        .await
        .expect("background completion")
        .expect("completion callback");
    assert!(!result.success);
    assert!(!result.timed_out);
    let failure = result
        .subagent_failure
        .as_ref()
        .expect("background result keeps typed child failure");
    let diagnostic = failure.diagnostic().expect("model diagnostic");
    assert_eq!(diagnostic.status(), fixture.expected_status());
    assert_eq!(
        diagnostic.request_id(),
        fixture.expected_request_id().as_deref()
    );
    assert_eq!(diagnostic.provider(), Some("openai-compatible"));
    assert_eq!(provider.request_count(), fixture.expected_requests());

    let encoded_result = serde_json::to_vec(&result).expect("background result JSON");
    let decoded_result: BackgroundTaskResult =
        serde_json::from_slice(&encoded_result).expect("background result roundtrip");
    assert_eq!(decoded_result.child_thread_id, result.child_thread_id);
    let decoded_failure = decoded_result
        .subagent_failure
        .as_ref()
        .expect("roundtrip keeps background safe failure");
    assert_eq!(
        decoded_failure
            .diagnostic()
            .expect("model diagnostic")
            .status(),
        fixture.expected_status()
    );
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
        .position(|event| matches!(event.payload(), ExecutorEvent::SubagentStarted { .. }))
        .expect("background child start event");
    let stop_positions: Vec<_> = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event.payload(), ExecutorEvent::SubagentStopped { .. }).then_some(index)
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
    } = lifecycle[stop_positions[0]].payload()
    {
        assert_eq!(
            stop_failure
                .diagnostic()
                .expect("model diagnostic")
                .status(),
            fixture.expected_status()
        );
        assert_eq!(
            stop_failure
                .diagnostic()
                .expect("model diagnostic")
                .request_id(),
            fixture.expected_request_id().as_deref()
        );
    } else {
        panic!("background error stop must carry its safe failure facts");
    }
    // The error branch delivers the terminal BackgroundTaskResult through the
    // callback/TaskManager after SubagentStopped; it intentionally does not
    // synthesize a BackgroundTaskCompleted event on the event channel.
    assert!(!lifecycle
        .iter()
        .any(|event| matches!(event.payload(), ExecutorEvent::BackgroundTaskCompleted(_))));

    if let Some(status) = fixture.expected_status() {
        let notification = result.to_notification();
        assert!(notification.contains(&format!("model_error_status: {status}")));
        assert!(notification.contains(&format!("model_error_request_id: req-{status}")));
    }
    assert!(!result.to_notification().contains("provider body"));

    if matches!(fixture, FailureFixture::VisibleDeltaThenInterruption) {
        let delta_position = lifecycle
            .iter()
            .position(|event| matches!(event.payload(), ExecutorEvent::TextChunk { .. }))
            .expect("visible delta event");
        assert!(
            delta_position < stop_positions[0],
            "visible output must precede the terminal stop: events={lifecycle:?}"
        );
    }

    BackgroundFailure {
        result,
        lifecycle,
        provider,
    }
}

#[tokio::test]
async fn background_http_400_is_not_retried_and_keeps_safe_identity() {
    let fixture = run_background_fixture(FailureFixture::Http(400)).await;
    let diagnostic = fixture.diagnostic();
    assert_eq!(diagnostic.category_name(), "http_status");
    assert_eq!(diagnostic.status(), Some(400));
    assert_eq!(diagnostic.request_id(), Some("req-400"));
    assert_eq!(fixture.provider.request_count(), 1, "HTTP 400 不可重试");
}

#[tokio::test]
async fn background_http_500_retries_and_keeps_safe_identity() {
    let fixture = run_background_fixture(FailureFixture::Http(500)).await;
    let diagnostic = fixture.diagnostic();
    assert_eq!(diagnostic.category_name(), "http_status");
    assert_eq!(diagnostic.status(), Some(500));
    assert_eq!(diagnostic.request_id(), Some("req-500"));
    assert_eq!(fixture.provider.request_count(), 10);
}

#[tokio::test]
async fn background_http_429_consumes_typed_result_and_safe_notification() {
    let fixture = run_background_fixture(FailureFixture::Http(429)).await;
    let diagnostic = fixture.diagnostic();
    assert_eq!(diagnostic.category_name(), "http_status");
    assert_eq!(diagnostic.status(), Some(429));
    assert_eq!(diagnostic.request_id(), Some("req-429"));
    assert_eq!(fixture.provider.request_count(), 10);
}

#[tokio::test]
async fn background_visible_delta_then_interruption_forwards_delta_before_stop() {
    let fixture = run_background_fixture(FailureFixture::VisibleDeltaThenInterruption).await;
    let diagnostic = fixture.diagnostic();
    assert_eq!(diagnostic.category_name(), "stream_interrupted");
    assert!(diagnostic.status().is_none());
    assert!(diagnostic.request_id().is_none());
    let delta = fixture
        .lifecycle
        .iter()
        .find_map(|event| match event.payload() {
            ExecutorEvent::TextChunk { chunk, .. } => Some(chunk.as_str()),
            _ => None,
        });
    assert_eq!(delta, Some("visible-before-interruption"));
}
