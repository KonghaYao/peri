//! HTTP body 中断的日志保真契约；独立 integration target 隔离 tracing callsite 捕获。

use peri_agent::{
    agent::{
        events_v2::EventBus,
        model_bridge::AgentModelBridge,
        stages::{run_react_loop, LoopResult, StageContext},
    },
    error::AgentError,
    messages::BaseMessage,
    middleware::MiddlewareChain,
    session::{FrozenContext, MessageSource, QueuedMessage, Session},
};
use peri_model::{OpenAiConfig, OpenAiModel, RetryObservation, RetryObserver};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

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
        self.observations.lock().unwrap().push(observation.clone());
        self.downstream
            .as_ref()
            .is_some_and(|observer| observer.on_interrupted(observation))
    }
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
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
    use peri_agent::session::retry_events::{retry_observer_for, RetryEventForwarder};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handler = Arc::new(FnEventHandler(|_| {}));
    let forwarder = RetryEventForwarder::new();
    forwarder.set(Some(handler.clone()));
    let observers: Vec<Option<Arc<dyn peri_model::RetryObserver>>> = vec![
        None,
        Some(Arc::new(|_: peri_model::RetryObservation| {})),
        Some(retry_observer_for(handler)),
        Some(forwarder.as_retry_observer()),
        Some(RetryEventForwarder::new().as_retry_observer()),
    ];
    for (observer, record_observations) in observers
        .into_iter()
        .map(|observer| (observer, true))
        .chain(std::iter::once((None, false)))
    {
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
            let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
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
            let observations = observations.lock().unwrap();
            assert_eq!(
                observations.len(),
                if record_observations { lines.len() } else { 0 }
            );
            for (line, observation) in lines.iter().zip(observations.iter()) {
                let diagnostic = observation
                    .diagnostic()
                    .expect("interruption has a diagnostic");
                let provider = format!("provider={:?}", diagnostic.provider().unwrap_or("unknown"));
                assert!(line.contains(&provider), "{logs}");
                let category = format!("error_kind={:?}", diagnostic.category_name());
                assert!(line.contains(&category), "{logs}");
                let transport = format!(
                    "transport={:?}",
                    diagnostic.transport().unwrap().to_string()
                );
                assert!(line.contains(&transport), "{logs}");
                assert!(line.contains(diagnostic.message().unwrap()), "{logs}");
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
        let read = socket.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "请求体必须完整送达");
        bytes.extend_from_slice(&chunk[..read]);
        let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let length: usize = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .expect("模型请求应声明 Content-Length")
            .1
            .trim()
            .parse()
            .unwrap();
        let start = header_end + 4;
        if bytes.len() >= start + length {
            return serde_json::from_slice(&bytes[start..start + length]).unwrap();
        }
    }
}

async fn run_log_script(
    responses: Vec<Vec<u8>>,
    observer: Option<Arc<dyn RetryObserver>>,
) -> LoopResult {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/", listener.local_addr().unwrap());
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
        OpenAiConfig::new(endpoint.parse().unwrap(), "fixture-key", "fixture-model")
            .with_runtime(runtime),
    );
    let directory = tempfile::tempdir().unwrap();
    let session = Session::new(
        Arc::from(directory.path().to_str().unwrap()),
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
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("finish the task"),
    ));
    let serve = async move {
        let mut requests = Vec::new();
        for body in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_request(&mut socket).await);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len() + usize::from(body == PARTIAL.as_bytes())
            );
            socket
                .write_all(&[header.as_bytes(), &body].concat())
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
        }
        requests
    };
    // 同一作用域驱动客户端和服务端，超时会同时释放两端，不留下后台任务。
    let (result, _requests) = timeout(Duration::from_secs(5), async {
        tokio::join!(run_react_loop(ctx.clone(), 4), serve)
    })
    .await
    .expect("必须在有界时间内完成续跑及 HTTP fixture");
    result
}
