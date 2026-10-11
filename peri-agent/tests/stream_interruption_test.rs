//! 真实 HTTP/SSE → provider → bridge → Agent 循环的流中断恢复契约。

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use peri_agent::{
    agent::{
        events_v2::{EventBus, RenderEvent},
        model_bridge::AgentModelBridge,
        react::AgentOutput,
        stages::{run_react_loop, LoopResult, StageContext},
    },
    error::{AgentError, AgentResult},
    messages::BaseMessage,
    middleware::{capabilities::AfterAgentState, Middleware, MiddlewareChain},
    session::{FrozenContext, MessageSource, QueuedMessage, Session},
};
use peri_model::{OpenAiConfig, OpenAiModel};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const PARTIAL: &str =
    "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
const COMPLETE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"continued\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

struct CompletionCounter(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Middleware for CompletionCounter {
    fn name(&self) -> &str {
        "completion_counter"
    }

    async fn after_agent(
        &self,
        _: &mut dyn AfterAgentState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(output.clone())
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

struct Evidence {
    result: LoopResult,
    requests: Vec<Value>,
    messages: Vec<BaseMessage>,
    chunks: Vec<String>,
    completions: usize,
}

async fn run_script(responses: Vec<Vec<u8>>) -> Evidence {
    run_script_with_observer(responses, None, false).await
}

async fn run_script_with_observer(
    responses: Vec<Vec<u8>>,
    observer: Option<Arc<dyn peri_model::RetryObserver>>,
    truncated_transport: bool,
) -> Evidence {
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
    let completions = Arc::new(AtomicUsize::new(0));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(CompletionCounter(completions.clone())));
    let (bus, mut handles) = EventBus::new(Default::default());
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
                body.len() + usize::from(truncated_transport && body == PARTIAL.as_bytes())
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
    let (result, requests) = timeout(Duration::from_secs(5), async {
        tokio::join!(run_react_loop(ctx.clone(), 4), serve)
    })
    .await
    .expect("必须在有界时间内完成续跑及 HTTP fixture");
    let mut chunks = Vec::new();
    while let Some(event) = handles.try_render() {
        if let RenderEvent::TextChunk { chunk, .. } = event {
            chunks.push(chunk);
        }
    }
    Evidence {
        result,
        requests,
        messages: ctx.visible_messages(),
        chunks,
        completions: completions.load(Ordering::SeqCst),
    }
}

/// [回归测试] 非正常 SSE 结束必须发起携带部分正文和可信提醒的新请求，不能静默结束。
#[tokio::test]
async fn test_stream_interruption_resumes_over_http() {
    for tail in [
        b"data: [DONE]\n\n".as_slice(),
        b"data: {\"error\":{\"message\":\"upstream failed\"}}\n\ndata: [DONE]\n\n",
        b"data: {invalid\n\n",
        b"data: \xff\n\n",
    ] {
        let evidence = run_script(vec![
            [PARTIAL.as_bytes(), tail].concat(),
            COMPLETE.as_bytes().to_vec(),
        ])
        .await;
        assert!(
            matches!(evidence.result, LoopResult::Completed),
            "{:?}",
            evidence.result
        );
        assert_eq!(evidence.requests.len(), 2, "续跑必须发起第二次请求");
        let messages = evidence.requests[1]["messages"].as_array().unwrap();
        assert!(
            messages.iter().any(|message| {
                message["role"] == "assistant" && message["content"] == "partial"
            }),
            "第二次请求必须保留断点正文"
        );
        assert!(
            messages.iter().any(|message| {
                message["role"] == "user" && message.to_string().contains("stream_interrupted")
            }),
            "续跑提醒必须通过模型请求边界"
        );
        assert_eq!(
            evidence.chunks,
            ["partial", "continued"],
            "部分正文不能重复渲染"
        );
        assert_eq!(evidence.completions, 1, "仅真正完成时运行完成 hook");
        let answers: Vec<_> = evidence
            .messages
            .iter()
            .filter(|message| matches!(message, BaseMessage::Ai { .. }))
            .map(|message| message.content().to_string())
            .collect();
        assert_eq!(answers, ["partial", "continued"], "规范历史保留两轮回复");
    }
}

/// [回归测试] 假 DONE 连续出现时仍服从恢复预算，不得成为成功或无限续跑。
#[tokio::test]
async fn test_stream_interruption_http_recovery_exhausts_budget() {
    let body = format!("{PARTIAL}data: [DONE]\n\n").into_bytes();
    let evidence = run_script(vec![body.clone(), body]).await;
    assert!(
        matches!(
            evidence.result,
            LoopResult::Error(AgentError::StreamRecoveryExhausted { attempts: 2, .. })
        ),
        "{:?}",
        evidence.result
    );
    assert_eq!(evidence.requests.len(), 2);
    assert_eq!(evidence.chunks, ["partial", "partial"]);
    assert_eq!(evidence.completions, 0, "预算耗尽不能当作任务完成");
}

/// [回归测试] 空白结束原因不能让 Agent 提前退出，必须保存断点并请求续跑。
#[tokio::test]
async fn test_stream_interruption_blank_finish_reason_resumes_over_http() {
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"choices": [{"delta": {"content": "partial"}, "finish_reason": "  "}]})
    );
    let evidence = run_script(vec![body.into_bytes(), COMPLETE.as_bytes().to_vec()]).await;
    assert!(
        matches!(evidence.result, LoopResult::Completed),
        "{:?}",
        evidence.result
    );
    assert_eq!(evidence.requests.len(), 2, "空白结束原因必须触发续跑");
    let messages = evidence.requests[1]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| { message["role"] == "assistant" && message["content"] == "partial" }),
        "续跑必须携带断点正文"
    );
    assert_eq!(evidence.chunks, ["partial", "continued"]);
    assert_eq!(evidence.completions, 1);
}
