//! Real HTTP/SSE -> Responses codec -> Agent bridge -> RCRA terminal semantics.

use std::{
    collections::BTreeMap,
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
    tools::{BaseTool, ToolContext},
};
use peri_model::{ModelRuntimeConfig, ResponsesConfig, ResponsesModel, RetryConfig};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const PARTIAL: &str = "已保留的部分回答";

struct CountingTool(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl BaseTool for CountingTool {
    fn name(&self) -> &str {
        "Count"
    }
    fn description(&self) -> &str {
        "Count actual tool executions"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn is_direct(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _: Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("executed".into())
    }
}

struct CompletionCounter(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Middleware for CompletionCounter {
    fn name(&self) -> &str {
        "responses_completion_counter"
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

fn event(kind: &str, mut value: Value) -> String {
    value["type"] = json!(kind);
    format!("event: {kind}\ndata: {value}\n\n")
}

fn text_item(text: &str) -> Value {
    json!({"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
        "content": [{"type": "output_text", "text": text, "annotations": []}]})
}

fn call_item(arguments: &str) -> Value {
    json!({"id": "fc_item", "type": "function_call", "call_id": "call_execute", "name": "Count",
        "arguments": arguments, "status": "completed"})
}

fn partial_stream() -> String {
    [
        event("response.created", json!({"response": {"id": "resp_1", "status": "in_progress", "output": []}})),
        event("response.output_item.added", json!({"output_index": 0, "item": {"id": "msg_1", "type": "message", "role": "assistant", "status": "in_progress", "content": []}})),
        event("response.content_part.added", json!({"output_index": 0, "content_index": 0, "item_id": "msg_1", "part": {"type": "output_text", "text": "", "annotations": []}})),
        event("response.output_text.delta", json!({"output_index": 0, "content_index": 0, "item_id": "msg_1", "delta": PARTIAL})),
        event("response.output_item.added", json!({"output_index": 1, "item": {"id": "fc_item", "type": "function_call", "call_id": "call_execute", "name": "Count", "arguments": "", "status": "in_progress"}})),
        event("response.function_call_arguments.delta", json!({"output_index": 1, "item_id": "fc_item", "delta": "{"})),
    ].concat()
}

fn completed_stream(text: &str, call: Option<&str>) -> String {
    let mut output = vec![text_item(text)];
    if let Some(arguments) = call {
        output.push(call_item(arguments));
    }
    event(
        "response.completed",
        json!({"response": {"id": "resp_done", "status": "completed", "output": output,
        "usage": {"input_tokens": 10, "output_tokens": 5}}}),
    )
}

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = socket.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "HTTP request must contain its full body");
        bytes.extend_from_slice(&chunk[..read]);
        assert!(
            bytes.len() < 1_000_000,
            "fixture request unexpectedly large"
        );
        let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&bytes[..end]).unwrap();
        assert!(headers.starts_with("POST /v1/responses HTTP/1.1"));
        let length: usize = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap();
        if bytes.len() >= end + 4 + length {
            return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
        }
    }
}

struct Evidence {
    result: LoopResult,
    requests: Vec<Value>,
    messages: Vec<BaseMessage>,
    events: Vec<RenderEvent>,
    executions: usize,
    completions: usize,
    queue_empty: bool,
}

async fn run_script(responses: Vec<String>) -> Evidence {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/", listener.local_addr().unwrap());
    let runtime = ModelRuntimeConfig::default().with_retry(
        RetryConfig::default()
            .with_max_attempts(3)
            .with_base_delay(Duration::ZERO)
            .with_jitter(false),
    );
    let model = ResponsesModel::new(
        ResponsesConfig::new(endpoint.parse().unwrap(), "fixture-key", "fixture-model")
            .with_runtime(runtime),
    );
    let directory = tempfile::tempdir().unwrap();
    let session = Session::new(
        Arc::from(directory.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    let executions = Arc::new(AtomicUsize::new(0));
    let completions = Arc::new(AtomicUsize::new(0));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(CompletionCounter(completions.clone())));
    let mut tools: BTreeMap<String, Arc<dyn BaseTool>> = BTreeMap::new();
    tools.insert("Count".into(), Arc::new(CountingTool(executions.clone())));
    let (bus, mut handles) = EventBus::new(Default::default());
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::new(Arc::new(model))))
    .with_tools(Arc::new(parking_lot::RwLock::new(tools)))
    .with_middleware_chain(Arc::new(chain))
    .with_event_bus(Arc::new(bus))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("Count once, then finish"),
    ));
    let mut requests = Vec::new();
    let result = timeout(Duration::from_secs(5), async {
        let serve = async {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read_request(&mut socket).await);
                let body = &responses[(requests.len() - 1).min(responses.len() - 1)];
                let header = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        };
        tokio::select! {
            result = run_react_loop(context.clone(), 4) => result,
            _ = serve => unreachable!("fixture serves until the Agent finishes"),
        }
    }).await.expect("terminal handling must finish without another model request");
    let mut events = Vec::new();
    while let Some(event) = handles.try_render() {
        events.push(event);
    }
    Evidence {
        result,
        requests,
        messages: context.visible_messages(),
        events,
        executions: executions.load(Ordering::SeqCst),
        completions: completions.load(Ordering::SeqCst),
        queue_empty: context.session.queue.is_empty(),
    }
}

#[tokio::test]
async fn quota_failure_after_text_and_partial_tool_is_terminal_over_real_http() {
    let body = partial_stream()
        + &event(
            "response.failed",
            json!({"response": {"id": "resp_1", "status": "failed",
        "error": {"code": "usage_limit_exceeded", "message": "fixture plan quota exhausted"}}}),
        );
    let evidence = run_script(vec![body]).await;
    assert!(
        matches!(&evidence.result, LoopResult::Error(AgentError::ModelError(error)) if error.is_terminal_provider_failure()),
        "{:?}",
        evidence.result
    );
    assert_eq!(
        evidence.requests.len(),
        1,
        "terminal failure must not retry or auto-continue"
    );
    assert_eq!(
        evidence.executions, 0,
        "an unfinished tool must never execute"
    );
    assert_eq!(
        evidence.completions, 0,
        "terminal failure is not successful completion"
    );
    assert!(
        evidence.queue_empty,
        "failure must not enqueue a continuation"
    );
    let answers: Vec<_> = evidence
        .messages
        .iter()
        .filter(|message| matches!(message, BaseMessage::Ai { .. }))
        .map(|message| message.content().to_string())
        .collect();
    assert_eq!(
        answers,
        [PARTIAL],
        "persist the partial answer exactly once"
    );
    assert!(evidence.events.iter().any(|event| matches!(event, RenderEvent::ToolStarted { tool_call_id, .. } if tool_call_id == "call_execute")));
    assert!(evidence.events.iter().any(|event| matches!(event, RenderEvent::ToolEnded { tool_call_id, is_error: true, .. } if tool_call_id == "call_execute")), "close the provisional tool indicator");
}

#[tokio::test]
async fn invalid_completed_tool_arguments_never_dispatch_over_real_http() {
    let evidence = run_script(vec![completed_stream("", Some("{\"broken\":"))]).await;
    assert!(
        matches!(&evidence.result, LoopResult::Error(AgentError::ModelError(error)) if error.is_terminal_provider_failure()),
        "{:?}",
        evidence.result
    );
    assert_eq!(evidence.requests.len(), 1);
    assert_eq!(evidence.executions, 0);
    assert_eq!(evidence.completions, 0);
}

/// Positive control: the same real harness executes a valid tool once and replays
/// its native call ID with the result before a final completed response.
#[tokio::test]
async fn completed_tool_executes_once_and_reaches_followup_over_real_http() {
    let evidence = run_script(vec![
        completed_stream("", Some("{}")),
        completed_stream("done", None),
    ])
    .await;
    assert!(
        matches!(evidence.result, LoopResult::Completed),
        "{:?}",
        evidence.result
    );
    assert_eq!(evidence.executions, 1);
    assert_eq!(evidence.completions, 1);
    assert_eq!(evidence.requests.len(), 2);
    let input = evidence.requests[1]["input"].as_array().unwrap();
    assert_eq!(
        input
            .iter()
            .filter(|item| item["type"] == "function_call" && item["call_id"] == "call_execute")
            .count(),
        1
    );
    assert_eq!(
        input
            .iter()
            .filter(
                |item| item["type"] == "function_call_output" && item["call_id"] == "call_execute"
            )
            .count(),
        1
    );
}
