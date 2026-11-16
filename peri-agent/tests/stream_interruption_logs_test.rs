//! HTTP body 中断的日志保真契约；独立 integration target 隔离 tracing callsite 捕获。

use peri_agent::agent::events_v2::EventBus;
use peri_agent::agent::model_bridge::AgentModelBridge;

use peri_agent::agent::stages::LoopResult;
use peri_agent::agent::stages::StageContext;
use peri_agent::error::AgentError;
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::MiddlewareChain;
use peri_agent::session::FrozenContext;
use peri_agent::session::MessageSource;
use peri_agent::session::QueuedMessage;
use peri_agent::session::Session;
use peri_model::{OpenAiConfig, OpenAiModel, RetryObservation, RetryObserver};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

const PARTIAL: &str =
    "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
const COMPLETE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"continued\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

struct RecordingObserver {
    downstream: Option<Arc<dyn RetryObserver>>,
    observations: Arc<std::sync::Mutex<Vec<RetryObservation>>>,
}

impl RetryObserver for RecordingObserver {
    fn on_retry(&self, observation: RetryObservation) {
        if let Some(observer) = &self.downstream {
            observer.on_retry(observation);
        }
    }

    fn on_interrupted(&self, observation: RetryObservation) -> bool {
        self.observations
            .lock()
            .expect("self observations mutex must remain unpoisoned throughout the test")
            .push(observation.clone());
        self.downstream
            .as_ref()
            .is_some_and(|observer| observer.on_interrupted(observation))
    }
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("self lock mutex must remain unpoisoned throughout the test")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// [回归测试] HTTP body 提前 EOF 的底层诊断必须逐出口保真，日志捕获独立于恢复测试进程。
#[test]
fn interrupted_logs_preserve_observed_transport_diagnostics_once_per_exit() {
    use peri_acp_types::event::FnEventHandler;
    use peri_agent::session::retry_events;
    use peri_agent::session::retry_events::RetryEventForwarder;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the build fixture result is required for this assertion");
    let handler = Arc::new(FnEventHandler(|_| {}));
    let forwarder = RetryEventForwarder::new();
    forwarder.set(Some(handler.clone()));
    let observers: Vec<Option<Arc<dyn peri_model::RetryObserver>>> = vec![
        None,
        Some(Arc::new(|_: peri_model::RetryObservation| {})),
        Some(retry_events::retry_observer_for(handler)),
        Some(forwarder.as_retry_observer()),
        Some(RetryEventForwarder::new().as_retry_observer()),
    ];
    let paired_items = observers
        .into_iter()
        .map(|observer| (observer, true))
        .chain(std::iter::once((None, false)));
    for (observer, record_observations) in paired_items {
        for exhausted in [false, true] {
            let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
            let recording = Arc::new(RecordingObserver {
                downstream: observer.clone(),
                observations: Arc::clone(&observations),
            });
            let buffer = LogBuffer::default();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_ansi(false)
                .without_time()
                .with_writer(buffer.clone())
                .finish();
            let evidence = tracing::subscriber::with_default(subscriber, || {
                runtime.block_on(run_log_script(
                    vec![
                        PARTIAL.as_bytes().to_vec(),
                        if exhausted { PARTIAL } else { COMPLETE }
                            .as_bytes()
                            .to_vec(),
                    ],
                    record_observations.then_some(recording as Arc<dyn RetryObserver>),
                ))
            });
            if exhausted {
                assert!(matches!(
                    evidence,
                    LoopResult::Error(AgentError::StreamRecoveryExhausted { attempts: 2, .. })
                ));
            } else {
                assert!(matches!(evidence, LoopResult::Completed));
            }
            let logs = String::from_utf8(
                buffer
                    .0
                    .lock()
                    .expect("buffer lock mutex must remain unpoisoned throughout the test")
                    .clone(),
            )
            .expect("the fixture text must be valid UTF-8");
            let lines: Vec<_> = logs
                .lines()
                .filter(|line| line.contains("模型流中断"))
                .collect();
            assert_eq!(
                lines.len(),
                if exhausted { 2 } else { 1 },
                "每个抑制出口恰好一条日志: {logs}"
            );
            for line in &lines {
                assert!(
                    line.contains("WARN") && line.contains("stream_interrupted"),
                    "{logs}"
                );
                assert!(
                    line.contains("attempts=1") && line.contains("max_attempts=2"),
                    "{logs}"
                );
                for field in ["provider=", "error_kind=", "transport="] {
                    assert!(line.contains(field), "missing diagnostic field: {logs}");
                }
            }
            let observations = observations
                .lock()
                .expect("observations lock mutex must remain unpoisoned throughout the test");
            assert_eq!(
                observations.len(),
                if record_observations { lines.len() } else { 0 }
            );
            let paired_items_2 = lines.iter().zip(observations.iter());
            for (line, observation) in paired_items_2 {
                let diagnostic = observation
                    .diagnostic()
                    .expect("interruption has a diagnostic");
                let provider = format!("provider={:?}", diagnostic.provider().unwrap_or("unknown"));
                assert!(line.contains(&provider), "{logs}");
                let category = format!("error_kind={:?}", diagnostic.category_name());
                assert!(line.contains(&category), "{logs}");
                let transport = format!(
                    "transport={:?}",
                    diagnostic
                        .transport()
                        .expect("the transport fixture result is required for this assertion")
                        .to_string()
                );
                assert!(line.contains(&transport), "{logs}");
                assert!(
                    line.contains(
                        diagnostic
                            .message()
                            .expect("the fixture entry must contain a message payload")
                    ),
                    "{logs}"
                );
                assert!(!diagnostic.causes().is_empty());
                for cause in diagnostic.causes() {
                    assert!(line.contains(cause), "missing diagnostic cause: {logs}");
                }
            }
            for forbidden in [
                "fixture-key",
                "finish the task",
                "partial",
                "continued",
                "choices",
                "Bearer",
                "Authorization",
            ] {
                assert!(!logs.contains(forbidden), "日志泄漏请求或响应内容");
            }
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = socket
            .read(&mut chunk)
            .await
            .expect("the local HTTP fixture connection must yield its request bytes");
        assert_ne!(read, 0, "请求体必须完整送达");
        bytes.extend_from_slice(&chunk[..read]);
        let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&bytes[..header_end])
            .expect("the fixture text must be valid UTF-8");
        let length: usize = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .expect("模型请求应声明 Content-Length")
            .1
            .trim()
            .parse()
            .expect("the parse fixture result is required for this assertion");
        let start = header_end + 4;
        if bytes.len() >= start + length {
            return serde_json::from_slice(&bytes[start..start + length])
                .expect("from_slice must accept this test fixture representation");
        }
    }
}

async fn run_log_script(
    responses: Vec<Vec<u8>>,
    observer: Option<Arc<dyn RetryObserver>>,
) -> LoopResult {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the bind fixture result is required for this assertion");
    let endpoint = format!(
        "http://{}/v1/",
        listener
            .local_addr()
            .expect("the local addr fixture result is required for this assertion")
    );
    let mut runtime = peri_model::ModelRuntimeConfig::default().with_retry(
        peri_model::RetryConfig::default()
            .with_max_attempts(2)
            .with_base_delay(Duration::ZERO)
            .with_jitter(false),
    );
    if let Some(observer) = observer {
        runtime = runtime.with_retry_observer(observer);
    }
    let model = OpenAiModel::new(
        OpenAiConfig::new(
            endpoint
                .parse()
                .expect("the parse fixture result is required for this assertion"),
            "fixture-key",
            "fixture-model",
        )
        .with_runtime(runtime),
    );
    let directory = tempfile::tempdir().expect("the temporary test directory must be available");
    let session = Session::new(
        Arc::from(
            directory
                .path()
                .to_str()
                .expect("the fixture text must be valid UTF-8"),
        ),
        FrozenContext::builder().build(),
        None,
    );
    let chain = MiddlewareChain::new();
    let (bus, _handles) = EventBus::new(Default::default());
    let ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::new(Arc::new(model))))
    .with_middleware_chain(Arc::new(chain))
    .with_event_bus(Arc::new(bus))
    .build();
    ctx.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("finish the task"),
    ));
    let serve = async move {
        let mut requests = Vec::new();
        for body in responses {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("the accept fixture result is required for this assertion");
            requests.push(read_request(&mut socket).await);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len() + usize::from(body == PARTIAL.as_bytes())
            );
            socket
                .write_all(&[header.as_bytes(), &body].concat())
                .await
                .expect("the write all fixture result is required for this assertion");
            socket
                .shutdown()
                .await
                .expect("the shutdown fixture result is required for this assertion");
        }
        requests
    };
    // 同一作用域驱动客户端和服务端，超时会同时释放两端，不留下后台任务。
    let (result, _requests) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            peri_agent::agent::stages::run_react_loop(ctx.clone(), 4),
            serve
        )
    })
    .await
    .expect("必须在有界时间内完成续跑及 HTTP fixture");
    result
}
